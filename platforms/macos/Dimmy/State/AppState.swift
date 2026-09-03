import AppKit
import ApplicationServices
import AVFoundation
import Combine
import IOKit.hid
import SwiftUI

// MARK: - PermissionsManager (single source of truth for macOS privacy permissions)

/// Live view of the macOS TCC state for the permissions Dimmy needs.
/// - Reads the system on every `refresh()` — never assumes or caches stale state.
/// - Auto-refreshes on `NSApplication.didBecomeActiveNotification` (user returns from System Settings)
///   plus a low-frequency timer as a safety net.
/// - Exposes request methods that trigger the native prompts. Separate from the status getters so
///   UI can display real state even when the user has not yet clicked a "Grant" button.
@MainActor
final class PermissionsManager: ObservableObject {
    static let shared = PermissionsManager()

    @Published private(set) var microphone: AVAuthorizationStatus = .notDetermined
    @Published private(set) var accessibility: Bool = false
    @Published private(set) var inputMonitoring: IOHIDAccessType = kIOHIDAccessTypeUnknown

    /// True when the permissions strictly required for Dimmy's core loop are granted.
    /// Microphone (record) + Accessibility (active CGEventTap + paste via CGEvent).
    /// Input Monitoring is NOT strictly required because `.defaultTap` on `.cgSessionEventTap`
    /// is gated by Accessibility, not Input Monitoring — but we still surface it so users whose
    /// machines route modifier events through HID pipelines have a one-click fix.
    var allRequiredGranted: Bool {
        microphone == .authorized && accessibility
    }

    var microphoneGranted: Bool { microphone == .authorized }
    var accessibilityGranted: Bool { accessibility }
    var inputMonitoringGranted: Bool { inputMonitoring == kIOHIDAccessTypeGranted }

    private var pollTimer: Timer?
    private var didBecomeActiveObserver: NSObjectProtocol?

    private init() {
        refresh()
        didBecomeActiveObserver = NotificationCenter.default.addObserver(
            forName: NSApplication.didBecomeActiveNotification,
            object: nil,
            queue: .main
        ) { [weak self] _ in
            Task { @MainActor in self?.refresh() }
        }
        pollTimer = Timer.scheduledTimer(withTimeInterval: 5.0, repeats: true) { [weak self] _ in
            Task { @MainActor in self?.refresh() }
        }
    }

    /// Re-query TCC directly. Safe to call often; updates published properties only on change.
    func refresh() {
        let newMic = AVCaptureDevice.authorizationStatus(for: .audio)
        let newAx = AXIsProcessTrustedWithOptions(nil)
        let newIm = IOHIDCheckAccess(kIOHIDRequestTypeListenEvent)
        if newMic != microphone { microphone = newMic }
        if newAx != accessibility { accessibility = newAx }
        if newIm != inputMonitoring { inputMonitoring = newIm }
    }

    /// Explicit refresh intended for user-action sites (button clicks, post-dialog).
    /// Identical to `refresh()`; separate name signals intent at the call site.
    func refreshNow() {
        refresh()
    }

    /// Trigger the native microphone prompt. No-op if the user has already decided.
    /// Returns the final status after the user responds (or immediately if already decided).
    @discardableResult
    func requestMicrophone() async -> Bool {
        if microphone == .notDetermined {
            _ = await AVCaptureDevice.requestAccess(for: .audio)
        }
        refresh()
        return microphoneGranted
    }

    /// Show the native Accessibility prompt. If the user clicks "Open System Settings",
    /// they're taken to Privacy & Security → Accessibility with Dimmy pre-selected.
    func promptAccessibility() {
        let key = kAXTrustedCheckOptionPrompt.takeRetainedValue()
        let options = [key: true] as CFDictionary
        _ = AXIsProcessTrustedWithOptions(options)
        refresh()
    }

    /// Deep-link to System Settings → Privacy & Security → Accessibility.
    func openAccessibilitySettings() {
        guard let url = URL(string: "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility") else { return }
        NSWorkspace.shared.open(url)
    }

    /// Wipe TCC entries for this bundle and service so the next grant request creates a fresh
    /// record matching the current binary's code signature. Works around macOS's tendency to
    /// keep stale entries keyed on old signatures (common when developing ad-hoc signed builds
    /// — System Settings shows the app as granted but `AXIsProcessTrustedWithOptions` returns
    /// false because the running process's signature matches a different/missing entry).
    /// Captures tccutil's exit code + output so a silent failure is no longer
    /// invisible. Returns true only if every reset reported success.
    ///
    /// IMPORTANT (why the old reset buttons "did nothing"): the status APIs
    /// `AXIsProcessTrustedWithOptions`, `AVCaptureDevice.authorizationStatus`
    /// and `IOHIDCheckAccess` are latched for the lifetime of THIS process.
    /// After a reset, `refresh()` cannot observe the change and the checkmarks
    /// won't update — the app must be relaunched (`relaunchApp()`). tccutil DID
    /// run; the live checks just couldn't reflect it. Callers must prompt for
    /// a relaunch.
    @discardableResult
    func resetTccEntries(services: [String]) -> Bool {
        let bundleId = Bundle.main.bundleIdentifier ?? "com.dimmy.app"
        var allOK = true
        for service in services {
            let process = Process()
            process.launchPath = "/usr/bin/tccutil"
            process.arguments = ["reset", service, bundleId]
            let pipe = Pipe()
            process.standardError = pipe
            process.standardOutput = pipe
            do {
                try process.run()
                let data = pipe.fileHandleForReading.readDataToEndOfFile()
                process.waitUntilExit()
                let out = String(data: data, encoding: .utf8)?
                    .trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
                let ok = process.terminationStatus == 0
                allOK = allOK && ok
                NSLog("[Permissions] tccutil reset %@ %@ -> exit=%d %@",
                      service, bundleId, process.terminationStatus, out)
            } catch {
                allOK = false
                NSLog("[Permissions] tccutil reset %@ could not launch: %@",
                      service, error.localizedDescription)
            }
        }
        refresh()
        return allOK
    }

    /// Quit + reopen the app. Required after a TCC reset because the
    /// permission-status APIs are latched per-process and won't reflect the
    /// new state until a fresh launch.
    func relaunchApp() {
        let url = Bundle.main.bundleURL
        let config = NSWorkspace.OpenConfiguration()
        config.createsNewApplicationInstance = true
        NSWorkspace.shared.openApplication(at: url, configuration: config) { _, _ in
            DispatchQueue.main.async { NSApp.terminate(nil) }
        }
    }

    /// Trigger the Input Monitoring prompt. macOS shows its own dialog if status is unknown.
    func requestInputMonitoring() {
        _ = IOHIDRequestAccess(kIOHIDRequestTypeListenEvent)
        refresh()
    }

    /// Deep-link to System Settings → Privacy & Security → Input Monitoring.
    func openInputMonitoringSettings() {
        guard let url = URL(string: "x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent") else { return }
        NSWorkspace.shared.open(url)
    }

    deinit {
        if let o = didBecomeActiveObserver {
            NotificationCenter.default.removeObserver(o)
        }
        pollTimer?.invalidate()
    }
}

enum RecordingMode: String, CaseIterable {
    case pushToTalk = "Push-to-Talk"
    case toggle = "Toggle"
}

// MARK: - File-load progress payload

/// Mirror of the `file_transcribe_progress` event Rust emits between
/// chunks. The detail view binds `percent` to a determinate progress
/// bar; the chunk index/total drive the "X / Y" status text.
struct FileTranscribeProgress: Equatable {
    let processedSecs: Double
    let totalSecs: Double
    let percent: Double
    let chunkIndex: Int
    let chunkTotal: Int
}

enum AppTheme: String, CaseIterable {
    case auto = "Auto"
    case light = "Light"
    case dark = "Dark"
}

// MARK: - STT Preset (flat combo like Windows)

struct SttPreset: Identifiable, Hashable {
    let id: String  // unique key
    let displayName: String
    let provider: SttProvider
    let apiUrl: String
    let model: String

    /// Asset Catalog imageset name in `Assets.xcassets/Providers/`. Empty
    /// for `.custom` (the UI swaps in an SF Symbol fallback).
    var iconAssetName: String {
        switch provider {
        case .groq: return "groq"
        case .openai: return "openai"
        case .deepgram: return "deepgram"
        case .gemini: return "gemini"
        case .fireworks: return "fireworks"
        case .together: return "together"
        case .custom: return ""
        }
    }

    // Derived from the single-source model catalog (ModelCatalog, fed by
    // assets/model-catalog.json over FFI) so a model add/remove touches only
    // the JSON. Mirror of the Win SettingsViewModel.ProviderPresets. The
    // trailing Custom entry is kept in code; local models are separate.
    static let presets: [SttPreset] = buildPresets()

    private static func sttProvider(_ id: String) -> SttProvider {
        switch id {
        case "groq": return .groq
        case "openai": return .openai
        case "deepgram": return .deepgram
        case "gemini": return .gemini
        case "fireworks": return .fireworks
        case "together": return .together
        default: return .custom
        }
    }

    private static func buildPresets() -> [SttPreset] {
        var list = ModelCatalog.models(task: "stt").map { pair in
            SttPreset(
                id: "\(pair.provider.id)-\(pair.model.id)",
                displayName: "\(pair.provider.name) \u{00B7} \(pair.model.label)",
                provider: sttProvider(pair.provider.id),
                apiUrl: pair.provider.stt_url,
                model: pair.model.id)
        }
        list.append(SttPreset(id: "custom", displayName: "Custom endpoint", provider: .custom, apiUrl: "", model: ""))
        return list
    }

    static func find(url: String, model: String) -> SttPreset? {
        presets.first { $0.apiUrl == url && $0.model == model }
    }
}

// MARK: - LLM Preset (flat combo like Windows)

struct LlmPreset: Identifiable, Hashable {
    let id: String
    let displayName: String
    let apiUrl: String
    let model: String

    /// Asset Catalog imageset name in `Assets.xcassets/Providers/`, derived
    /// from the API URL host. Empty for unknown / custom URLs (the UI swaps
    /// in an SF Symbol fallback).
    var iconAssetName: String {
        if apiUrl.contains("groq.com") { return "groq" }
        if apiUrl.contains("openai.com") { return "openai" }
        if apiUrl.contains("openrouter.ai") { return "openrouter" }
        if apiUrl.contains("googleapis.com") { return "gemini" }
        if apiUrl.contains("anthropic.com") { return "anthropic" }
        if apiUrl.contains("fireworks.ai") { return "fireworks" }
        if apiUrl.contains("together.xyz") || apiUrl.contains("together.ai") { return "together" }
        return ""
    }

    // Derived from the single-source model catalog (ModelCatalog, fed by
    // assets/model-catalog.json over FFI) so a model add/remove touches only
    // the JSON. Mirror of the Win SettingsViewModel.LlmProviderPresets. The
    // Claude Code subscription synthetic + Custom are kept in code (not API
    // models); local models are injected separately.
    static let presets: [LlmPreset] = buildPresets()

    private static func buildPresets() -> [LlmPreset] {
        var list = ModelCatalog.models(task: "llm").map { pair in
            LlmPreset(
                id: "\(pair.provider.id)-\(pair.model.id)",
                displayName: "\(pair.provider.name) \u{00B7} \(pair.model.label)",
                apiUrl: pair.provider.llm_url,
                model: pair.model.id)
        }
        // Claude Code subscription — synthetic provider using the user's
        // Pro/Team/Max plan via the local `claude` CLI. No API key; auth via
        // `claude login`. Not an API model, so kept in code.
        // Synthetic `claude-code` and `openai-codex` presets were
        // removed 2026-06-20. They duplicated the Anthropic / OpenAI
        // provider rows with a fake "subscription" model and forced
        // the user's model choice to jump to a different label every
        // time they flipped the Authentication picker. The
        // subscription path is now signaled purely by
        // `llm_auth_method == "subscription"` — the user keeps
        // picking `Anthropic · <their model>` and the Authentication
        // picker chooses CLI vs API on top. Legacy URLs
        // (`claude-code://default`, `codex://default`) are migrated to
        // the real vendor URL on load in `loadFromRustConfig`.
        list.append(LlmPreset(id: "custom", displayName: "Custom endpoint", apiUrl: "", model: ""))
        return list
    }

    static func find(url: String, model: String) -> LlmPreset? {
        presets.first { $0.apiUrl == url && $0.model == model }
    }
}

enum RecordingState: Equatable {
    case idle
    case recording(RecordingMode)
    case transcribing
    case processing
    case completing
}

// MARK: - LLM Style (mirrors Rust LlmStyle, 13 variants)

enum LlmStyle: String, CaseIterable, Identifiable {
    case off
    case correct
    case summarize
    case elaborate
    case comprehensible
    case professional
    case prompt
    case genz
    case boomer
    case emoji
    case acronyms
    case imbruttito
    case custom

    var id: String { rawValue }

    var displayName: String {
        switch self {
        case .off: return "Off"
        case .correct: return "Correct"
        case .summarize: return "Summarize"
        case .elaborate: return "Elaborate"
        case .comprehensible: return "Comprehensible"
        case .professional: return "Professional"
        case .prompt: return "Prompt"
        case .genz: return "Gen Z"
        case .boomer: return "Boomer"
        case .emoji: return "Emoji"
        case .acronyms: return "Acronyms"
        case .imbruttito: return "Imbruttito"
        case .custom: return "Custom"
        }
    }

    /// Color associated with this style (for pill dot indicator)
    var color: Color {
        switch self {
        case .off: return .gray
        case .correct: return .blue
        case .summarize: return .purple
        case .elaborate: return .orange
        case .comprehensible: return .cyan
        case .professional: return .indigo
        case .prompt: return .mint
        case .genz: return .pink
        case .boomer: return .brown
        case .emoji: return .yellow
        case .acronyms: return .teal
        case .imbruttito: return .red
        case .custom: return .green
        }
    }
}

// MARK: - LLM Tone (mirrors Rust LlmTone, 5 variants)

enum LlmTone: String, CaseIterable, Identifiable {
    case none
    case formal
    case friendly
    case concise
    case academic

    var id: String { rawValue }

    var displayName: String {
        switch self {
        case .none: return "None"
        case .formal: return "Formal"
        case .friendly: return "Friendly"
        case .concise: return "Concise"
        case .academic: return "Academic"
        }
    }
}

// MARK: - STT Provider (mirrors Rust Provider enum)

enum SttProvider: String, CaseIterable, Identifiable {
    case groq
    case openai
    case deepgram
    case gemini
    case fireworks
    case together
    case custom

    var id: String { rawValue }

    var displayName: String {
        switch self {
        case .groq: return "Groq"
        case .openai: return "OpenAI"
        case .deepgram: return "Deepgram"
        case .gemini: return "Gemini"
        case .fireworks: return "Fireworks"
        case .together: return "Together"
        case .custom: return "Custom"
        }
    }

    var defaultApiUrl: String {
        switch self {
        case .groq: return "https://api.groq.com/openai/v1/audio/transcriptions"
        case .openai: return "https://api.openai.com/v1/audio/transcriptions"
        case .deepgram: return "https://api.deepgram.com/v1/listen"
        case .gemini: return "https://generativelanguage.googleapis.com/v1beta/models"
        case .fireworks: return "https://audio-turbo.api.fireworks.ai/v1/audio/transcriptions"
        case .together: return "https://api.together.xyz/v1/audio/transcriptions"
        case .custom: return ""
        }
    }

    var defaultModel: String {
        switch self {
        case .groq: return "whisper-large-v3-turbo"
        case .openai: return "whisper-1"
        case .deepgram: return "nova-2"
        case .gemini: return "gemini-2.0-flash"
        case .fireworks: return "whisper-v3-turbo"
        case .together: return "nvidia/parakeet-tdt-0.6b-v3"
        case .custom: return ""
        }
    }

    var models: [String] {
        switch self {
        case .groq: return ["whisper-large-v3-turbo", "whisper-large-v3"] // distil-whisper-large-v3-en decommissioned 2026-05-15
        case .openai: return ["whisper-1", "gpt-4o-transcribe", "gpt-4o-mini-transcribe"]
        case .deepgram: return ["nova-2", "nova-3"]
        case .gemini: return ["gemini-3.1-flash-lite", "gemini-3-flash-preview", "gemini-2.5-flash"]
        case .fireworks: return ["whisper-v3-turbo"]  // whisper-v3 large rejected by 2026-05-04 benchmark (too slow)
        case .together: return ["nvidia/parakeet-tdt-0.6b-v3", "openai/whisper-large-v3"]  // Voxtral skipped (lower match%)
        case .custom: return []
        }
    }

    static func from(url: String) -> SttProvider {
        if url.contains("groq.com") { return .groq }
        if url.contains("openai.com") { return .openai }
        if url.contains("deepgram.com") { return .deepgram }
        if url.contains("googleapis.com") { return .gemini }
        if url.contains("fireworks.ai") { return .fireworks }
        if url.contains("together.xyz") || url.contains("together.ai") { return .together }
        return .custom
    }
}

/// Represents a modifier-key-only shortcut (e.g., ⌃⌥ or Fn)
struct ModifierShortcut: Equatable {
    var fn: Bool
    var control: Bool
    var option: Bool
    var command: Bool
    var shift: Bool

    var displayString: String {
        displayParts.joined(separator: "")
    }

    var displayParts: [String] {
        var parts: [String] = []
        if fn { parts.append("fn") }
        if control { parts.append("⌃") }
        if option { parts.append("⌥") }
        if shift { parts.append("⇧") }
        if command { parts.append("⌘") }
        return parts
    }

    var isFnOnly: Bool {
        fn && !control && !option && !command && !shift
    }

    func matches(flags: NSEvent.ModifierFlags) -> Bool {
        let f = flags.intersection(.deviceIndependentFlagsMask)
        return f.contains(.function) == fn
            && f.contains(.control) == control
            && f.contains(.option) == option
            && f.contains(.command) == command
            && f.contains(.shift) == shift
    }

    /// Fn alone is valid; otherwise need 2+ modifiers
    var isValid: Bool {
        if fn && !control && !option && !command && !shift { return true }
        let count = [control, option, command, shift].filter { $0 }.count
        return count >= 2
    }

    /// Translate this shortcut into the Rust hotkey grammar consumed by
    /// `dimmy_hotkey_combos_conflict`. The Rust parser accepts
    /// `cmd|win`, `opt|alt`, `ctrl`, `shift` for modifiers. The `fn`
    /// (function) modifier has no Rust grammar token — fn-only shortcuts
    /// return `""` so the conflict check sees them as "no opinion" and
    /// the command hotkey is free to claim any combo.
    var rustGrammar: String {
        var parts: [String] = []
        if control { parts.append("ctrl") }
        if option { parts.append("opt") }
        if command { parts.append("cmd") }
        if shift { parts.append("shift") }
        // Need ≥2 tokens for the Rust parser to accept the input; a single
        // mod alone is not a valid grammar form.
        guard parts.count >= 2 else { return "" }
        return parts.joined(separator: "+")
    }

    static let fnOnly = ModifierShortcut(fn: true, control: false, option: false, command: false, shift: false)
    static let controlOption = ModifierShortcut(fn: false, control: true, option: true, command: false, shift: false)
    static let `default` = fnOnly

    // Persistence
    var encoded: Int {
        var val = 0
        if control { val |= 1 }
        if option { val |= 2 }
        if command { val |= 4 }
        if shift { val |= 8 }
        if fn { val |= 16 }
        return val
    }

    init(fn: Bool = false, control: Bool, option: Bool, command: Bool, shift: Bool) {
        self.fn = fn
        self.control = control
        self.option = option
        self.command = command
        self.shift = shift
    }

    init(encoded: Int) {
        self.control = encoded & 1 != 0
        self.option = encoded & 2 != 0
        self.command = encoded & 4 != 0
        self.shift = encoded & 8 != 0
        self.fn = encoded & 16 != 0
    }
}

/// Modifier+(optional) key chord. Used by:
/// - Add-to-dictionary hotkey (Cmd+Shift+D default) — always mod+key
/// - Dedicated one-shot Command-Mode hotkey — mod+key OR modifier-only
///
/// Modifier-only mode is signalled by `keyCode == nil` (`keyChar`
/// empty), mirroring the Win-side support for chords like Win+Alt that
/// the new Rust-hook command shortcut also accepts. The Win side
/// persists the equivalent string as `ctrl+shift+d` / `cmd+opt`; on Mac
/// we keep a structured form locally and emit the Win-style string only
/// when calling into the Rust hotkey grammar (`rustGrammar`).
struct HotkeyCombo: Equatable {
    var control: Bool
    var option: Bool
    var command: Bool
    var shift: Bool
    /// macOS virtual key code for the non-modifier key, when present.
    /// Letters: A=0, S=1, D=2, F=3, …  See HIToolbox/Events.h
    /// kVK_ANSI_* constants. `nil` = modifier-only combo (no key).
    var keyCode: UInt16?
    /// Display character for `keyCode` (uppercase letter / glyph). Empty
    /// when `keyCode` is nil. Stored separately so we don't have to
    /// maintain a keyCode→char table inside View code — the recorder
    /// writes both on capture.
    var keyChar: String

    var displayParts: [String] {
        var parts: [String] = []
        if control { parts.append("⌃") }
        if option { parts.append("⌥") }
        if shift { parts.append("⇧") }
        if command { parts.append("⌘") }
        if !keyChar.isEmpty { parts.append(keyChar) }
        return parts
    }

    var displayString: String { displayParts.joined(separator: "") }

    /// True when the combo has no non-modifier key (e.g. Win+Alt =
    /// Cmd+Option). Modifier-only combos activate on `.flagsChanged`
    /// only; mod+key combos require an additional `.keyDown` matching
    /// the `keyCode`.
    var isModifierOnly: Bool { keyCode == nil }

    /// Number of modifiers required by the combo. Used by the recorder
    /// to validate "at least one modifier" before saving.
    var modifierCount: Int {
        [control, option, command, shift].filter { $0 }.count
    }

    /// Match a keyDown event against this combo. Modifier-only combos
    /// never match a keyDown — the caller (`DictHotkeyManager`) handles
    /// only mod+key hotkeys via this path.
    func matches(flags: NSEvent.ModifierFlags, keyCode kc: UInt16) -> Bool {
        guard let code = keyCode else { return false }
        let f = flags.intersection(.deviceIndependentFlagsMask)
        return kc == code
            && f.contains(.control) == control
            && f.contains(.option) == option
            && f.contains(.command) == command
            && f.contains(.shift) == shift
    }

    /// Translate this combo into the Rust hotkey grammar consumed by
    /// `dimmy_hotkey_set_command` / `dimmy_hotkey_combos_conflict`. The
    /// Rust parser is platform-agnostic: it accepts `cmd|win`, `opt|alt`,
    /// `ctrl`, `shift` for modifiers and letter / function / special
    /// names for keys. Examples:
    ///   `Cmd+Shift+D`   → `"cmd+shift+d"`
    ///   `Cmd+Opt`       → `"cmd+opt"`         (modifier-only)
    ///   `Ctrl+Shift+F5` → `"ctrl+shift+f5"`
    /// Returns `""` for an "empty" combo (no modifiers and no key) so
    /// the Rust side treats it as disabled.
    var rustGrammar: String {
        var parts: [String] = []
        if control { parts.append("ctrl") }
        if option { parts.append("opt") }
        if command { parts.append("cmd") }
        if shift { parts.append("shift") }
        if let code = keyCode, let name = Self.macKeyCodeRustName(code) {
            parts.append(name)
        }
        if parts.isEmpty { return "" }
        // The Rust parser requires at least one '+' — a single token
        // would parse as empty / unrecognised. With at least 2 parts
        // (mod + key) or 2 mods, the join below always contains '+'.
        if parts.count == 1 {
            // Single modifier alone is invalid (the recorder rejects
            // 0-mod combos and the Rust parser needs 2 mods OR 1 mod +
            // key). Emit empty to signal "disabled".
            return ""
        }
        return parts.joined(separator: "+")
    }

    /// Mac kVK_* virtual key code → Rust hotkey grammar key name. Covers
    /// the same set the Rust `name_to_vk` parser accepts: A-Z, 0-9,
    /// F1-F12, Space/Tab/Enter/Esc/Backspace, numpad. Returns nil for
    /// unmapped codes (the recorder rejects unrecognised keys before
    /// saving, so unmapped paths shouldn't be reached at runtime).
    static func macKeyCodeRustName(_ code: UInt16) -> String? {
        switch Int(code) {
        case 0x00: return "a"
        case 0x01: return "s"
        case 0x02: return "d"
        case 0x03: return "f"
        case 0x04: return "h"
        case 0x05: return "g"
        case 0x06: return "z"
        case 0x07: return "x"
        case 0x08: return "c"
        case 0x09: return "v"
        case 0x0B: return "b"
        case 0x0C: return "q"
        case 0x0D: return "w"
        case 0x0E: return "e"
        case 0x0F: return "r"
        case 0x10: return "y"
        case 0x11: return "t"
        case 0x12: return "1"
        case 0x13: return "2"
        case 0x14: return "3"
        case 0x15: return "4"
        case 0x16: return "6"
        case 0x17: return "5"
        case 0x18: return "="
        case 0x19: return "9"
        case 0x1A: return "7"
        case 0x1C: return "8"
        case 0x1D: return "0"
        case 0x1F: return "o"
        case 0x20: return "u"
        case 0x22: return "i"
        case 0x23: return "p"
        case 0x25: return "l"
        case 0x26: return "j"
        case 0x28: return "k"
        case 0x2D: return "n"
        case 0x2E: return "m"
        case 0x24: return "enter"
        case 0x30: return "tab"
        case 0x31: return "space"
        case 0x33: return "backspace"
        case 0x35: return "esc"
        case 0x7A: return "f1"
        case 0x78: return "f2"
        case 0x63: return "f3"
        case 0x76: return "f4"
        case 0x60: return "f5"
        case 0x61: return "f6"
        case 0x62: return "f7"
        case 0x64: return "f8"
        case 0x65: return "f9"
        case 0x6D: return "f10"
        case 0x67: return "f11"
        case 0x6F: return "f12"
        default: return nil
        }
    }

    /// Mac default = ⌘⇧D — mirrors Win's Ctrl+Shift+D combo for
    /// muscle-memory consistency across platforms. macOS keyCode for
    /// "D" is 0x02 (kVK_ANSI_D).
    static let defaultDictHotkey = HotkeyCombo(
        control: false, option: false, command: true, shift: true,
        keyCode: 0x02, keyChar: "D"
    )

    /// Compact string form for UserDefaults round-trip. Two shapes:
    ///   - mod+key   : `cmd+shift+02:D`
    ///   - mod-only  : `cmd+opt` (no trailing `<hex>:<char>` token)
    /// On decode, the absence of a `:`-bearing trailing token signals
    /// the modifier-only case.
    var encoded: String {
        var parts: [String] = []
        if control { parts.append("ctrl") }
        if option { parts.append("opt") }
        if command { parts.append("cmd") }
        if shift { parts.append("shift") }
        if let code = keyCode {
            parts.append(String(format: "%02x:%@", code, keyChar))
        }
        return parts.joined(separator: "+")
    }

}

extension HotkeyCombo {
    /// Inverse of `encoded`. Returns nil for malformed input; callers
    /// should fall back to `defaultDictHotkey` (dict path) or `nil`
    /// (command path) in that case.
    ///
    /// Lives in an extension so Swift still synthesises the implicit
    /// memberwise initialiser (`HotkeyCombo(control:option:...)`) on
    /// the struct itself — adding a custom init directly on the struct
    /// suppresses memberwise synthesis, which silently broke
    /// `defaultDictHotkey` (the existing memberwise call site).
    init?(encoded: String) {
        let tokens = encoded.split(separator: "+").map(String.init)
        if tokens.isEmpty { return nil }
        // Detect mod-key vs mod-only by checking the trailing token for `:`.
        if let last = tokens.last, last.contains(":") {
            guard let colon = last.firstIndex(of: ":") else { return nil }
            let hex = String(last[..<colon])
            let char = String(last[last.index(after: colon)...])
            guard let kc = UInt16(hex, radix: 16) else { return nil }
            let mods = Set(tokens.dropLast())
            self.init(
                control: mods.contains("ctrl"),
                option: mods.contains("opt"),
                command: mods.contains("cmd"),
                shift: mods.contains("shift"),
                keyCode: kc,
                keyChar: char
            )
        } else {
            let mods = Set(tokens)
            // Require ≥1 known modifier name; otherwise treat as invalid
            // so a stray "foo" string doesn't yield an all-false combo.
            let known = ["ctrl", "opt", "cmd", "shift"]
            guard mods.contains(where: { known.contains($0) }) else { return nil }
            self.init(
                control: mods.contains("ctrl"),
                option: mods.contains("opt"),
                command: mods.contains("cmd"),
                shift: mods.contains("shift"),
                keyCode: nil,
                keyChar: ""
            )
        }
    }
}

// MARK: - Hotkey Status (surfaces CGEventTap install state to the UI)

/// Tracks whether the global shortcut interception is live.
/// Drives pill/menu-bar warning overlays and the Diagnostics pane.
enum HotkeyStatus: Equatable {
    case uninstalled            // app just launched, not yet attempted
    case installed              // CGEventTap active, shortcut works
    case accessibilityMissing   // Accessibility permission not granted
    case tapFailed(reason: String)  // unexpected install failure
}

@MainActor
final class AppState: ObservableObject {
    static let shared = AppState()

    // MARK: - Recording State

    @Published var recordingState: RecordingState = .idle
    @Published var preferredMode: RecordingMode = .pushToTalk
    @Published var waveformLevels: [CGFloat] = Array(repeating: 0.2, count: 7)
    /// Mirror of `dimmy_meeting_is_active()`. PillWindowController polls
    /// every 500 ms so the pill UI knows when a meeting started from
    /// any surface (meeting window, future tray menu) and can render
    /// the Stop button + recording feedback even with no dictation.
    @Published var meetingActive: Bool = false
    /// Captured at meeting START from `MeetingViewModel.generateRecap`
    /// (true for call-detect-started meetings). Every stop path —
    /// meeting window button, pill, call-detect "Stop" popup — reads
    /// THIS flag, NOT the live checkbox, so a reopened-window stale
    /// toggle can't override the user's choice at start, and the pill
    /// / popup paths stop running the recap unconditionally. Mac
    /// mirror of Win `AppViewModel.MeetingGenerateRecap`.
    @Published var meetingGenerateRecap: Bool = true
    /// Mirror of `dimmy_meeting_is_paused()`. Also polled from the pill
    /// state poll so the bar / status reflects pause state regardless
    /// of which surface flipped it.
    @Published var meetingIsPaused: Bool = false
    /// Absolute path of the meeting directory currently producing
    /// chunks — pinned on the first `meeting_chunk` event so later
    /// events from a stale recording (briefly tail-emitting after
    /// the next meeting starts) get dropped instead of mixing into
    /// the new transcript. Cleared by MeetingViewModel on
    /// `start()` / `stop()`.
    @Published var meetingActiveDir: String = ""
    /// Running concatenation of every `meeting_chunk` event's `line`
    /// for the active recording. Replaces the previous Win pattern
    /// of re-reading `transcripts.txt` from disk every 2 s — the
    /// Rust core already produces these lines, we just append on
    /// receive. MeetingViewModel binds its live-transcript view to
    /// this property.
    @Published var meetingLiveTranscript: String = ""
    /// Last `chunk_count` reported by the Rust meeting worker —
    /// drives the "N chunks" pill in the meeting recording UI.
    @Published var meetingChunkCount: Int = 0
    /// Last speaker tag from a `meeting_chunk` event ("mic" or
    /// "system"). Currently informational; future: per-speaker
    /// colour bars in the transcript view.
    @Published var meetingLastChunkSpeaker: String = ""
    /// Increments by 1 on every `meeting_chunk` event so views that
    /// can't bind directly to a String change (eg. attributed-text
    /// renderers that hold their own buffer) can observe a primitive
    /// counter and re-render. Same pattern as `liveCaptionTick`.
    @Published var meetingChunkTick: Int = 0
    @Published var lastTranscript: String = ""
    @Published var lastError: String?
    @Published var hotkeyStatus: HotkeyStatus = .uninstalled
    @Published var chunkProgress: (current: Int, total: Int)?

    var isRecording: Bool {
        if case .recording = recordingState { return true }
        return false
    }

    // MARK: - Onboarding & UI

    @Published var isOnboardingComplete: Bool {
        didSet { UserDefaults.standard.set(isOnboardingComplete, forKey: "isOnboardingComplete") }
    }
    /// Whether Dimmy shows a Dock icon outside of onboarding.
    /// Onboarding always forces the app into `.regular` activation policy so the user
    /// can find it again after clicking away to System Settings. This preference
    /// controls the post-onboarding steady state only.
    @Published var showInDock: Bool {
        didSet {
            UserDefaults.standard.set(showInDock, forKey: "showInDock")
            // Safety: never let both Dock and menu-bar disappear, the user
            // would lose every entry point to the app.
            if !showInDock && !showInMenuBar { showInMenuBar = true }
        }
    }
    /// Whether the menu-bar (NSStatusItem) icon is visible. Off-by-default
    /// is unsafe — the user could hide both Dock and menu bar and lose
    /// access to the app — so the setter on either property forces the
    /// other to stay on if it would result in zero visibility.
    @Published var showInMenuBar: Bool {
        didSet {
            UserDefaults.standard.set(showInMenuBar, forKey: "showInMenuBar")
            if !showInMenuBar && !showInDock { showInDock = true }
        }
    }
    @Published var theme: AppTheme = .auto
    @Published var shortcut: ModifierShortcut {
        didSet { UserDefaults.standard.set(shortcut.encoded, forKey: "shortcutEncoded") }
    }

    /// Last email entered in the pre-checkout / activate modal. Persisted
    /// across Sign out so the user doesn't have to re-type it. Distinct
    /// from license.json (which Sign out drops): this is just a UX
    /// convenience pre-fill, no auth weight. Mirrors UiPreferences.BuyerEmail
    /// on the Win side.
    @Published var buyerEmail: String? {
        didSet { UserDefaults.standard.set(buyerEmail, forKey: "buyerEmail") }
    }
    @Published var pillPosition: CGPoint? {
        didSet {
            if let pos = pillPosition {
                UserDefaults.standard.set(pos.x, forKey: "pillX")
                UserDefaults.standard.set(pos.y, forKey: "pillY")
            }
        }
    }
    @Published var showPillIntro: Bool = false

    // MARK: - Permissions


    // MARK: - STT Mode (local vs cloud)

    @Published var sttMode: String = "local"  // "local" or "cloud" — macOS defaults to local (no API key prompt during onboarding)
    @Published var localModel: String = "ggml-base-q8_0.bin"
    @Published var modelDownloadProgress: Double = 0.0
    @Published var isDownloadingModel: Bool = false
    @Published var fillerRemovalEnabled: Bool = true

    /// Active local STT backend. Mirrors core's AppConfig.local_stt_backend.
    /// "whisper" = whisper.cpp (ggml models, default); "parakeet" = NVIDIA
    /// TDT v3 FP32 via ONNX Runtime. The two are mutually exclusive — the
    /// dispatcher in transcribe.rs picks one based on this string.
    @Published var localSttBackend: String = "whisper"
    @Published var parakeetDownloadProgress: Double = 0.0
    @Published var isDownloadingParakeet: Bool = false
    @Published var parakeetBundlePresent: Bool = false

    /// Show the floating subtitle window during recording when the
    /// chunked engine is firing. Independent of `chunkStreamingEnabled`
    /// — power users can keep the chunked transcriber on (better tail
    /// latency on long recordings) while preferring no live captions.
    /// Defaults to true to match the Win shipping behaviour.
    @Published var liveCaptionsEnabled: Bool = true
    /// Per-chunk delta pushed by the Rust core via `stt_chunk` events.
    /// CaptionWindowController subscribes — UI publishers like a
    /// status pill could subscribe too.
    @Published var liveCaptionDelta: String = ""
    /// Cumulative running transcript during chunked recording. Reset
    /// when a new recording starts. Useful if any view wants the
    /// full text without re-accumulating from deltas.
    @Published var liveCaptionCumulative: String = ""
    /// Bumped each time an `stt_chunk` event lands so observers can
    /// react to "another chunk arrived" without diffing strings.
    @Published var liveCaptionTick: Int = 0
    @Published var liveCaptionIsFinal: Bool = false

    /// Recap/command text as the LLM writes it, accumulated from the core's
    /// `llm_stream` event. A slow open-weight model can take half a minute
    /// before its first word (measured: 35s batch vs 11.7s streamed on
    /// Kimi K3), and an empty panel for that long reads as a hang.
    /// OpenAI-compatible, Anthropic and Claude Code all stream; with
    /// Gemini-native this stays empty and the UI shows nothing.
    @Published var llmStreamText: String = ""

    /// The model's reasoning trace while it works, when the provider sends
    /// one (Anthropic with adaptive thinking, Claude Code via stream-json).
    /// Deliberately SEPARATE from `llmStreamText`: this is progress, never
    /// recap content. An opus recap runs past a minute before its first
    /// answer token, and this is what fills that silence.
    @Published var llmThinkingText: String = ""

    // MARK: - Call detection

    /// Toggle for the 1 Hz CoreAudio poll that detects VoIP calls and
    /// offers a "Record now" nudge bottom-right. Default true — mirror
    /// of Rust `Config::call_detect_enabled`. Round-trips via
    /// `dimmy_set_config_json` → Rust `state.call_detect_enabled`.
    @Published var callDetectEnabled: Bool = true

    /// Lowercase canonical app ids the user has chosen never to be
    /// nudged about (popup "Don't ask for X again"). Mirror of Rust
    /// `Config::call_detect_excluded_apps` (default `["discord"]`).
    /// Win surfaces this list in Settings → Auto-detect; Mac mirrors
    /// the same in MacVoicePage so users can re-enable apps without
    /// editing config.json.
    @Published var callDetectExcludedApps: [String] = []

    // MARK: - LLM Mode (local vs cloud)

    @Published var llmMode: String = "cloud"  // "local" or "cloud"
    @Published var localLlmModel: String = "gemma-4-E2B-it-Q4_K_M.gguf"
    @Published var llmModelDownloadProgress: Double = 0.0
    @Published var isDownloadingLlmModel: Bool = false

    // MARK: - STT Config (synced with Rust via FFI)

    @Published var sttProvider: SttProvider = .groq
    @Published var apiUrl: String = "https://api.groq.com/openai/v1/audio/transcriptions" {
        didSet {
            // Re-derive the STT green ✓ flag on every URL change so the
            // dropdown reflects "key saved for this provider" in real time.
            // applyConfigSnapshot loads per-provider flags AFTER apiUrl, so
            // a stale recompute can fire during the initial config load —
            // harmless because applyConfigSnapshot does a final recompute
            // at its tail. UI-driven provider changes always see the
            // up-to-date flags.
            if apiUrl != oldValue { hasKey = sttKeyPresent(forUrl: apiUrl) }
        }
    }
    @Published var apiModel: String = "whisper-large-v3-turbo"
    @Published var selectedLanguage: String = ""
    @Published var prompt: String = ""
    @Published var hasKey: Bool = false
    @Published var selectedDevice: String?
    @Published var devices: [String] = []

    // MARK: - LLM Config (synced with Rust via FFI)

    @Published var llmEnabled: Bool = false
    @Published var llmStyle: String = "off" {
        didSet { llmStyleEnum = LlmStyle(rawValue: llmStyle) ?? .off }
    }
    @Published var llmStyleEnum: LlmStyle = .off
    @Published var llmTone: String = "none" {
        didSet { llmToneEnum = LlmTone(rawValue: llmTone) ?? .none }
    }
    @Published var llmToneEnum: LlmTone = .none
    @Published var llmCustomPrompt: String = ""
    @Published var llmTranslateTo: String = "none"
    @Published var llmApiUrl: String = "" {
        didSet {
            if llmApiUrl != oldValue { hasLlmKey = llmKeyPresent(forUrl: llmApiUrl) }
        }
    }
    @Published var llmApiModel: String = ""
    @Published var llmUseSameKey: Bool = true
    @Published var hasLlmKey: Bool = false
    @Published var llmLogEnabled: Bool = false

    /// LLM auth method — `"api_key"` (default) or `"subscription"`.
    /// When `"subscription"`, the Rust dispatcher routes every cloud
    /// LLM call through the local `claude` CLI (Pro/Team/Max plan)
    /// regardless of `llmApiUrl`. Legacy `claude-code://` URLs are
    /// also honoured as a back-compat trigger.
    ///
    /// `recapAuthMethod` lets the meeting-recap call site override
    /// this independently: `""` inherits the dictation choice;
    /// `"api_key"` or `"subscription"` force a specific path for the
    /// recap only. This is the "fast dictation via API, recap via
    /// subscription" UX from the Win redesign.
    @Published var llmAuthMethod: String = "api_key"
    @Published var recapAuthMethod: String = ""

    /// "Use same key as LLM/STT" for the meeting-recap call. Mirrors
    /// `llmUseSameKey` one layer down — when ON (default), the recap
    /// dispatcher reuses an existing upstream key (LLM-scope first,
    /// STT-scope as fallback) for the recap vendor instead of asking
    /// for a dedicated Recap-scope key. Win parity (PR #62).
    @Published var recapUseSameKey: Bool = true

    /// Per-vendor "has key" snapshots for the LLM and Recap keystore
    /// scopes, mirrored from the Rust config JSON (`has_<vendor>_llm_key`,
    /// `has_<vendor>_recap_key`). Refreshed on every `loadConfig` so
    /// MacOutputPage's recap visibility matrix can answer "does an
    /// upstream key exist for this vendor?" in O(1) without re-reading
    /// the config snapshot on every render. STT-scope flags
    /// (`has_<vendor>_key`) are mirrored separately as individual
    /// `hasGroqKey` / `hasOpenaiKey` / … @Published vars above.
    @Published var llmKeyByVendor: [String: Bool] = [:]
    @Published var recapKeyByVendor: [String: Bool] = [:]

    /// Cached "is the Anthropic / Claude Code subscription connection
    /// usable right now" flag. Re-probed by `refreshClaudeCodeStatus()`
    /// (called from Settings pages on appear). Drives the gate on the
    /// LLM auth picker — Subscription is only offered when this is
    /// true. Cached so view bodies don't run the FFI probe on every
    /// redraw.
    @Published var claudeCodeReady: Bool = false

    /// Same shape as `claudeCodeReady` but for the OpenAI / ChatGPT
    /// subscription via the local `codex` CLI. Gates the Codex branch
    /// of the LLM Authentication picker on the Output page — parity
    /// with Win where the API-key / Subscription toggle is offered for
    /// both Anthropic AND OpenAI when the matching CLI is signed in.
    @Published var codexReady: Bool = false

    /// Re-probe Claude Code install + credential state. Cheap (a file
    /// stat + a Keychain query); safe to call from view onAppear /
    /// auth picker change. Updates `claudeCodeReady`.
    func refreshClaudeCodeStatus() {
        guard DimmyCore.shared.isInitialized else {
            claudeCodeReady = false
            return
        }
        // The probe shells out to `security find-generic-password` for
        // the Keychain check — on a cold launch that can take ~50-100 ms
        // (process spawn + Keychain unlock dialog auth). Run it off the
        // main thread so .onAppear callers in Settings pages don't
        // block their first paint, then bounce back to update the
        // @Published. Same dispatch pattern as MacVoicePage's
        // refreshLocalModelStatus().
        DispatchQueue.global(qos: .userInitiated).async {
            let ready = (DimmyCore.shared.claudeCodeStatus == .ready)
            DispatchQueue.main.async {
                self.claudeCodeReady = ready
            }
        }
    }

    /// Re-probe Codex install + ChatGPT sign-in state. Same dispatch
    /// pattern as `refreshClaudeCodeStatus` (the `codex` CLI also
    /// stores its OAuth token where a file-stat probe is enough).
    /// Drives the OpenAI / Codex branch of the LLM auth picker.
    func refreshCodexStatus() {
        guard DimmyCore.shared.isInitialized else {
            codexReady = false
            return
        }
        DispatchQueue.global(qos: .userInitiated).async {
            let ready = (DimmyCore.shared.codexStatus == .ready)
            DispatchQueue.main.async {
                self.codexReady = ready
            }
        }
    }

    // MARK: - Audio Config

    // Off, matching the core's default since 2026-08-31. The core is the single
    // writer of config.json, so an initial value that disagrees with it would
    // show the toggle ON while the audio path runs OFF.
    @Published var preprocessingEnabled: Bool = false
    @Published var chunkStreamingEnabled: Bool = false
    @Published var audioDebugEnabled: Bool = false
    @Published var inputGain: Float = 0.5

    // MARK: - UI State

    @Published var showAdvanced: Bool = false
    @Published var useKeyring: Bool = false

    /// Feature flag for the Tahoe-redesign Settings UI. Defaults to ON
    /// during the redesign sprint so internal builds get the new look;
    /// flip to false in the Settings scene to fall back to the legacy
    /// `SettingsContainerView` if a regression turns up. Persisted in
    /// UserDefaults so QA can pin one or the other across launches.
    @Published var useTahoeSettings: Bool = UserDefaults.standard.object(forKey: "useTahoeSettings") as? Bool ?? true {
        didSet { UserDefaults.standard.set(useTahoeSettings, forKey: "useTahoeSettings") }
    }

    /// User preference for whether the floating pill is visible. Off by
    /// preference for users who already have the menubar item — toggled
    /// from the menubar popover. Persists across launches.
    @Published var pillVisible: Bool = UserDefaults.standard.object(forKey: "pillVisible") as? Bool ?? true {
        didSet { UserDefaults.standard.set(pillVisible, forKey: "pillVisible") }
    }

    // MARK: - Appearance Config

    @Published var borderStyle: String = "Rainbow"
    @Published var waveformStyle: String = "Bars"
    @Published var overlayPosition: String = "Bottom Right"
    @Published var keepInClipboard: Bool = false

    /// Command Mode: when true, the next recording transforms the user's
    /// CURRENTLY-SELECTED text using what they speak as the instruction,
    /// instead of dictating. Runtime-only (a transient mode, not persisted)
    /// — toggled from the status-bar / pill menu and reset on restart so a
    /// forgotten toggle can't surprise the user across sessions. Mirrors
    /// Win's `AppViewModel.CommandMode`.
    @Published var commandMode: Bool = false

    /// One-shot Command Mode: set true when the user fires the dedicated
    /// command hotkey. Lives orthogonal to `commandMode` (the sticky
    /// menu toggle) so menu-flipped users and hotkey-flipped users
    /// never block each other. Cleared at the end of the next
    /// `stopAndCommandTransform` cycle. The pill amber + every
    /// "is command mode active" gate reads `commandMode || oneShotCommandPending`.
    @Published var oneShotCommandPending: Bool = false

    // MARK: - Stats

    @Published var statsTotalWords: UInt64 = 0
    @Published var statsTotalSpeakingSecs: Double = 0.0

    // MARK: - History v2

    /// Save 16 kHz mono int16 WAV alongside each row in
    /// `<config>/history_audio/<id>.wav`. Off by default — opt-in for
    /// users who want playback. Round-trips through the Rust core.
    @Published var saveAudioInHistory: Bool = false
    /// Days to keep audio recordings before the prune thread deletes
    /// them. 0 = never delete by age.
    @Published var historyAudioKeepDays: UInt32 = 30
    /// Maximum total size of `<config>/history_audio/` in MB. When the
    /// dir exceeds this, the prune thread deletes oldest WAVs first
    /// until under the cap. 0 = no size cap.
    @Published var historyAudioMaxMb: UInt32 = 5_000

    // MARK: - Phase 6.4 — auto-recap

    /// Auto-generate a recap when a single dictation runs longer than
    /// this many seconds (fire-and-forget, after paste, prepended to
    /// `enhanced_text`). 0 = disabled. Default 60.
    @Published var autoRecapThresholdSecs: UInt32 = 60

    /// User-picked recap model from the curated dropdown in Mac
    /// Settings → Advanced. Empty string = Auto (uses the
    /// pickRecapModel() heuristic — match LLM provider). Non-empty
    /// values flow through `dimmy_llm_call_raw(model_override=...)`
    /// for both meeting recap and auto-recap. Mirror of Win
    /// `SettingsWindow` recap-model ComboBox stored as
    /// `recap_model_override` in config.json.
    @Published var recapModelOverride: String = ""

    /// User-chosen meeting storage directory (absolute path). Empty = the
    /// default `<configdir>/meetings`. Mirror of Win `MeetingStoragePath`.
    /// The effective dir (override + writability fallback) is resolved by
    /// the Rust core via `DimmyCore.shared.meetingsDirURL`; this field is
    /// just the raw override for display + persistence.
    @Published var meetingStoragePath: String = ""

    // MARK: - Notion integration

    /// True if a Notion integration token is currently stored. Driven
    /// by `has_notion_token` in the Rust config snapshot. Read-only
    /// from the UI's perspective — the token itself round-trips via
    /// the dedicated `dimmy_notion_set_token` FFI, never via config.
    @Published var hasNotionToken: Bool = false

    /// UUID of the parent Notion page or database where meeting recaps
    /// land. Empty = onboarding state in the Settings UI.
    @Published var notionTargetId: String = ""

    /// "page" or "database" — drives the Notion API request shape.
    @Published var notionTargetKind: String = ""

    /// Display name of the picked target ("Meeting Notes",
    /// "Engineering log") shown in Settings as confirmation. Pure UI.
    @Published var notionTargetTitle: String = ""

    /// True = every meeting auto-uploads to Notion at recap completion.
    /// False = user clicks the "Send to Notion" button per meeting.
    @Published var notionAutoSend: Bool = false

    // MARK: - Custom vocabulary / user dictionary

    /// Words / short phrases the user has flagged to boost in STT.
    /// Mirrored from the Rust `user_dict` config field; persisted via
    /// `toRustConfig()`. Order is preserved for UI display but doesn't
    /// matter for the STT bias itself (the Rust `compose_stt_prompt`
    /// joins them with commas before passing to Whisper / cloud /
    /// Gemini / Deepgram).
    @Published var userDictWords: [String] = []

    /// Global hotkey that triggers "copy selection + add to dictionary".
    /// Default Cmd+Shift+D. Same grammar / parser as `shortcut` but used
    /// by `DictHotkeyManager` (separate CGEventTap, doesn't conflict
    /// with the main dictation hotkey). Persisted to UserDefaults
    /// rather than config.json because it's a Mac-only UI knob —
    /// the Rust core has no opinion on which key adds to the dict,
    /// only on what's IN the dict.
    @Published var dictHotkey: HotkeyCombo = .defaultDictHotkey {
        didSet { UserDefaults.standard.set(dictHotkey.encoded, forKey: "dictHotkeyEncoded") }
    }

    /// Dedicated one-shot Command-Mode hotkey. Nil = not configured;
    /// HotkeyManager registers a Carbon hotkey only when this is set
    /// AND the combo is valid. Pressing it flips
    /// `oneShotCommandPending = true` and starts a recording cycle —
    /// the next stop transforms the selection (or, with nothing
    /// selected, generates from the spoken instruction via
    /// `dimmy_command_transform`; the old paste-raw-spoken fallback was
    /// removed in d678061), then clears the flag. The sticky
    /// `commandMode` menu toggle is independent and never auto-cleared
    /// by this path. Mac-only knob; persisted to UserDefaults like
    /// `dictHotkey`.
    @Published var commandHotkey: HotkeyCombo? = nil {
        didSet {
            if let h = commandHotkey {
                UserDefaults.standard.set(h.encoded, forKey: "commandHotkeyEncoded")
            } else {
                UserDefaults.standard.removeObject(forKey: "commandHotkeyEncoded")
            }
        }
    }

    /// Optional global shortcut that TOGGLES a meeting recording (start ↔
    /// stop). `HotkeyManager` matches it on the shared CGEventTap; the pressed
    /// edge runs the consent-gated `MeetingShortcut.toggle`. EMPTY by default
    /// (opt-in) — a meeting-record key easily collides with whatever app is
    /// focused during a call (e.g. ⌃⇧M / ⌘⇧M is Teams mute). Toggle-only (a
    /// meeting can't be push-to-talk). Mac-only knob; persisted to UserDefaults
    /// like `commandHotkey`.
    @Published var meetingHotkey: HotkeyCombo? = nil {
        didSet {
            if let h = meetingHotkey {
                UserDefaults.standard.set(h.encoded, forKey: "meetingHotkeyEncoded")
            } else {
                UserDefaults.standard.removeObject(forKey: "meetingHotkeyEncoded")
            }
        }
    }

    // MARK: - App rules (foreground-app overrides)

    /// User-curated rules evaluated top-down at hotkey-down. Persisted
    /// as `app_rules` in the Rust config.json. The mac UI is in
    /// `MacRulesPage`; defaults live in `AppRulesDefaults.macV1`.
    @Published var appRules: [AppRule] = []

    // MARK: - File-load progress

    /// Live progress for `dimmy_transcribe_file`. nil while idle.
    @Published var fileTranscribeProgress: FileTranscribeProgress?

    // MARK: - Per-provider key flags

    @Published var hasGroqKey: Bool = false
    @Published var hasOpenaiKey: Bool = false
    @Published var hasOpenrouterKey: Bool = false
    @Published var hasGeminiKey: Bool = false
    @Published var hasDeepgramKey: Bool = false
    @Published var hasFireworksKey: Bool = false
    @Published var hasTogetherKey: Bool = false
    @Published var hasCustomKey: Bool = false

    // MARK: - Language list (display name → language code for Rust)

    // "" = auto-detect. The first block is supported by BOTH local backends
    // (whisper + Parakeet TDT v3, all European); the "(Whisper)" block is
    // whisper-only. Auto-detect covers whisper's other ~84 languages. Kept in
    // sync with Windows `SettingsViewModel.Languages` + Linux `LANGUAGES`.
    static let languageMap: [(display: String, code: String)] = [
        ("Auto-detect", ""),
        ("Italiano", "it"),
        ("English", "en"),
        ("Español", "es"),
        ("Français", "fr"),
        ("Deutsch", "de"),
        ("Português", "pt"),
        ("Nederlands", "nl"),
        ("Polski", "pl"),
        ("Русский", "ru"),
        ("Українська", "uk"),
        ("中文 (Whisper)", "zh"),
        ("日本語 (Whisper)", "ja"),
        ("한국어 (Whisper)", "ko"),
        ("العربية (Whisper)", "ar"),
        ("हिन्दी (Whisper)", "hi"),
    ]

    let languages: [String] = languageMap.map(\.display)

    /// System's preferred language mapped to a supported Whisper code, or `""` if unsupported.
    /// Used as a fallback default during onboarding to avoid Whisper's unreliable auto-detect
    /// on short clips (it misfires on < ~2s of speech and can produce empty transcripts).
    static func systemPreferredLanguageCode() -> String {
        let code = Locale.current.language.languageCode?.identifier ?? ""
        return languageMap.contains(where: { $0.code == code }) ? code : ""
    }

    // MARK: - Init

    private init() {
        self.isOnboardingComplete = UserDefaults.standard.bool(forKey: "isOnboardingComplete")
        self.showInDock = UserDefaults.standard.bool(forKey: "showInDock")
        // Default true: a fresh user sees the menu-bar icon, so they can
        // always find the app even if Dock is hidden.
        self.showInMenuBar = UserDefaults.standard.object(forKey: "showInMenuBar") as? Bool ?? true
        let savedX = UserDefaults.standard.double(forKey: "pillX")
        let savedY = UserDefaults.standard.double(forKey: "pillY")
        if savedX != 0 || savedY != 0 {
            self.pillPosition = CGPoint(x: savedX, y: savedY)
        }
        let savedShortcut = UserDefaults.standard.integer(forKey: "shortcutEncoded")
        if savedShortcut != 0 {
            self.shortcut = ModifierShortcut(encoded: savedShortcut)
        } else {
            self.shortcut = .default
        }
        if let saved = UserDefaults.standard.string(forKey: "dictHotkeyEncoded"),
           let combo = HotkeyCombo(encoded: saved) {
            self.dictHotkey = combo
        }
        if let saved = UserDefaults.standard.string(forKey: "commandHotkeyEncoded"),
           let combo = HotkeyCombo(encoded: saved) {
            self.commandHotkey = combo
        }
        if let saved = UserDefaults.standard.string(forKey: "meetingHotkeyEncoded"),
           let combo = HotkeyCombo(encoded: saved) {
            self.meetingHotkey = combo
        } else if !UserDefaults.standard.bool(forKey: "meetingHotkeyDefaultApplied") {
            // First run: default to ⌃⌥M (Ctrl+Alt+M), mirroring the Windows
            // default. NOT ⌃⇧M / ⌘⇧M — that's Teams' mute. Applied once (the
            // flag prevents resurrecting it after the user clears it). didSet
            // doesn't fire during init, so persist the encoded value explicitly.
            UserDefaults.standard.set(true, forKey: "meetingHotkeyDefaultApplied")
            let combo = HotkeyCombo(
                control: true, option: true, command: false, shift: false,
                keyCode: 0x2E, keyChar: "M"
            )
            UserDefaults.standard.set(combo.encoded, forKey: "meetingHotkeyEncoded")
            self.meetingHotkey = combo
        }
        self.buyerEmail = UserDefaults.standard.string(forKey: "buyerEmail")
    }

    // MARK: - Sync from Rust config JSON

    /// Load all config fields from the Rust FFI config JSON.
    func loadFromRustConfig(_ config: [String: Any]) {
        // STT
        if let url = config["api_url"] as? String {
            apiUrl = url
            sttProvider = SttProvider.from(url: url)
        }
        if let model = config["api_model"] as? String { apiModel = model }
        if let lang = config["language"] as? String {
            // First-run guard: Rust defaults language="" (auto-detect), but Whisper's language
            // detection is unreliable on short audio (<2s) and often misfires — a 1-second Italian
            // clip can be classified as Turkish, producing empty transcripts. Seed the language
            // from the system locale on first onboarding so new users get sensible results
            // without digging into Settings. Users can still pick "Auto-detect" explicitly later.
            let effectiveLang = (lang.isEmpty && !isOnboardingComplete)
                ? Self.systemPreferredLanguageCode()
                : lang
            selectedLanguage = Self.displayLanguage(for: effectiveLang)
        }
        if let p = config["prompt"] as? String { prompt = p }
        // `has_key` (session-active global) was the source of the
        // "phantom key saved" bug on the green ✓ badge — it stayed `true`
        // for any provider once any STT key existed in the session.
        // Win mirror dropped it (ViewModels/SettingsViewModel.cs). On Mac we
        // recompute `hasKey` further down from the per-provider STT flags
        // resolved against the current `apiUrl`, after `apiUrl` itself
        // has been loaded from the snapshot.
        if let dev = config["selected_device"] as? String { selectedDevice = dev }
        if let devs = config["devices"] as? [String] { devices = devs }

        // Local STT
        if let v = config["stt_mode"] as? String { sttMode = v }
        if let v = config["local_model"] as? String { localModel = v }
        if let v = config["local_stt_backend"] as? String { localSttBackend = v }
        if let v = config["live_captions_enabled"] as? Bool { liveCaptionsEnabled = v }
        if let v = config["filler_removal_enabled"] as? Bool { fillerRemovalEnabled = v }
        if let v = config["call_detect_enabled"] as? Bool { callDetectEnabled = v }
        if let arr = config["call_detect_excluded_apps"] as? [String] {
            callDetectExcludedApps = arr.map { $0.lowercased() }
        }

        // Local LLM
        if let v = config["llm_mode"] as? String { llmMode = v }
        if let v = config["local_llm_model"] as? String { localLlmModel = v }

        // Shortcut
        if let mode = config["shortcut_mode"] as? String {
            preferredMode = mode == "hold" ? .pushToTalk : .toggle
        }

        // LLM
        if let e = config["llm_enabled"] as? Bool { llmEnabled = e }
        if let s = config["llm_style"] as? String { llmStyle = s }
        if let t = config["llm_tone"] as? String { llmTone = t }
        if let cp = config["llm_custom_prompt"] as? String { llmCustomPrompt = cp }
        if let tt = config["llm_translate_to"] as? String { llmTranslateTo = tt }
        if let u = config["llm_api_url"] as? String { llmApiUrl = u }
        if let m = config["llm_api_model"] as? String { llmApiModel = m }
        if let sk = config["llm_use_same_key"] as? Bool { llmUseSameKey = sk }
        // `has_llm_key` (session-active global) had the same bug — recomputed
        // from per-provider LLM flags + `llmApiUrl` further down.
        if let ll = config["llm_log_enabled"] as? Bool { llmLogEnabled = ll }
        if let am = config["llm_auth_method"] as? String { llmAuthMethod = am }
        if let ram = config["recap_auth_method"] as? String { recapAuthMethod = ram }
        // Back-compat: migrate the legacy synthetic LLM URLs to the
        // real vendor URL + `llm_auth_method=subscription`. Old configs
        // (pre 2026-06-20) used `claude-code://default` and
        // `codex://default` as "CLI-routed" sentinels; the LLM
        // dropdown then needed two synthetic preset entries to render
        // them, which forced the user's model choice to jump every
        // time they flipped the Authentication picker. The new model:
        // URL stays on the real vendor endpoint, `auth_method` is the
        // sole subscription signal. Rust dispatcher already accepts
        // `(anthropic_url, "subscription")` → claude CLI and
        // `(openai_url, "subscription")` → codex CLI
        // (`llm.rs::process_text` lines 564-615), so this migration is
        // purely a UI-side cleanup.
        if llmApiUrl.hasPrefix("claude-code://") {
            llmAuthMethod = "subscription"
            llmApiUrl = "https://api.anthropic.com/v1/messages"
            if llmApiModel.isEmpty { llmApiModel = "claude-opus-4-7" }
        }
        if llmApiUrl.hasPrefix("codex://") {
            llmAuthMethod = "subscription"
            llmApiUrl = "https://api.openai.com/v1/chat/completions"
            if llmApiModel.isEmpty { llmApiModel = "gpt-5" }
        }

        // Audio
        if let pe = config["preprocessing_enabled"] as? Bool { preprocessingEnabled = pe }
        if let cs = config["chunk_streaming_enabled"] as? Bool { chunkStreamingEnabled = cs }
        if let ad = config["audio_debug_enabled"] as? Bool { audioDebugEnabled = ad }
        if let ig = config["input_gain"] as? Double { inputGain = Float(ig) }

        // Appearance
        if let bs = config["border_style"] as? String { borderStyle = bs }
        if let ws = config["waveform_style"] as? String { waveformStyle = ws }
        if let op = config["overlay_position"] as? String { overlayPosition = op }
        if let kc = config["keep_in_clipboard"] as? Bool { keepInClipboard = kc }

        // Stats
        if let tw = config["stats_total_words"] as? UInt64 { statsTotalWords = tw }
        else if let tw = config["stats_total_words"] as? Int { statsTotalWords = UInt64(tw) }
        if let ts = config["stats_total_speaking_secs"] as? Double { statsTotalSpeakingSecs = ts }

        // Keyring — always local encrypted file, ignore stored value
        useKeyring = false

        // Per-provider key flags — STT scope (`has_<vendor>_key`)
        if let v = config["has_groq_key"] as? Bool { hasGroqKey = v }
        if let v = config["has_openai_key"] as? Bool { hasOpenaiKey = v }
        if let v = config["has_openrouter_key"] as? Bool { hasOpenrouterKey = v }
        if let v = config["has_gemini_key"] as? Bool { hasGeminiKey = v }
        if let v = config["has_deepgram_key"] as? Bool { hasDeepgramKey = v }
        if let v = config["has_fireworks_key"] as? Bool { hasFireworksKey = v }
        if let v = config["has_together_key"] as? Bool { hasTogetherKey = v }
        if let v = config["has_custom_key"] as? Bool { hasCustomKey = v }

        // Per-vendor key flags — LLM scope (`has_<vendor>_llm_key`) and
        // Recap scope (`has_<vendor>_recap_key`). Drives the recap
        // visibility matrix in MacOutputPage (use-same-key toggle vs
        // dedicated recap key field). Mirrors Win _llmKeyByProvider +
        // HasRecapKey lookups.
        let llmVendors = ["groq", "openai", "anthropic", "gemini",
                          "openrouter", "fireworks", "together", "custom"]
        var llmMap: [String: Bool] = [:]
        var recapMap: [String: Bool] = [:]
        for v in llmVendors {
            if let b = config["has_\(v)_llm_key"] as? Bool { llmMap[v] = b }
            if let b = config["has_\(v)_recap_key"] as? Bool { recapMap[v] = b }
        }
        llmKeyByVendor = llmMap
        recapKeyByVendor = recapMap

        if let v = config["recap_use_same_key"] as? Bool { recapUseSameKey = v }

        // History v2 retention
        if let v = config["save_audio_in_history"] as? Bool { saveAudioInHistory = v }
        if let v = config["history_audio_keep_days"] as? Int { historyAudioKeepDays = UInt32(max(0, v)) }
        if let v = config["history_audio_max_mb"] as? Int { historyAudioMaxMb = UInt32(max(0, v)) }

        // Phase 6.4 auto-recap
        if let v = config["auto_recap_threshold_secs"] as? Int { autoRecapThresholdSecs = UInt32(max(0, v)) }
        if let v = config["recap_model_override"] as? String { recapModelOverride = v }
        if let v = config["meeting_storage_path"] as? String { meetingStoragePath = v }

        // Notion integration — see top of file.
        if let v = config["has_notion_token"] as? Bool { hasNotionToken = v }
        if let v = config["notion_target_id"] as? String { notionTargetId = v }
        if let v = config["notion_target_kind"] as? String { notionTargetKind = v }
        if let v = config["notion_target_title"] as? String { notionTargetTitle = v }
        if let v = config["notion_auto_send"] as? Bool { notionAutoSend = v }
        if let arr = config["user_dict"] as? [String] {
            // Rust strips empty / whitespace entries already; defensive
            // copy keeps the UI list rendering predictable.
            userDictWords = arr.filter { !$0.trimmingCharacters(in: .whitespaces).isEmpty }
        }

        // App rules — array of dicts matching Rust serde shape
        if let arr = config["app_rules"] as? [[String: Any]] {
            appRules = arr.compactMap { AppRule(dict: $0) }
        } else {
            appRules = []
        }

        // Derive `hasKey` + `hasLlmKey` from the per-provider flags using
        // the current `apiUrl` / `llmApiUrl` as the selector. Must run AFTER
        // both URLs and all per-provider flags have been populated above —
        // the order matters, don't move this earlier. Win mirror is in
        // ViewModels/SettingsViewModel.cs (HasSttKeyForUrl/HasLlmKeyForUrl).
        hasKey = sttKeyPresent(forUrl: apiUrl)
        hasLlmKey = llmKeyPresent(forUrl: llmApiUrl)
    }

    /// Resolve the STT key flag for a given API URL. Returns `true` only when
    /// the keystore actually has a key for the URL's vendor — fixes the
    /// pre-2026-05-18 "phantom green ✓" bug where the global `has_key`
    /// (session-active) stayed `true` for every provider once any STT key
    /// existed in the session.
    func sttKeyPresent(forUrl url: String) -> Bool {
        switch sttProviderTag(forUrl: url) {
        case "groq": return hasGroqKey
        case "openai": return hasOpenaiKey
        case "openrouter": return hasOpenrouterKey
        case "gemini": return hasGeminiKey
        case "deepgram": return hasDeepgramKey
        case "fireworks": return hasFireworksKey
        case "together": return hasTogetherKey
        default: return hasCustomKey
        }
    }

    /// Resolve the LLM key flag for a given LLM API URL. Same pattern as
    /// `sttKeyPresent(forUrl:)` but uses `llmKeyByVendor` populated from the
    /// `has_<vendor>_llm_key` Rust flags.
    func llmKeyPresent(forUrl url: String) -> Bool {
        let vendor = llmProviderTag(forUrl: url)
        return llmKeyByVendor[vendor] == true
    }

    /// Does ANY scope (LLM or STT) have a saved key for the LLM provider's
    /// vendor? Drives the "Use my saved API key for this provider" toggle
    /// visibility — show only when there's actually a key to reuse. Mirror
    /// of the Win `hasUpstreamKey` calculation in
    /// SettingsWindow.xaml.cs:RefreshAuthIntegrationStatus.
    func hasUpstreamKey(forLlmUrl url: String) -> Bool {
        let vendor = llmProviderTag(forUrl: url)
        guard !vendor.isEmpty else { return false }
        if llmKeyByVendor[vendor] == true { return true }
        switch vendor {
        case "groq": return hasGroqKey
        case "openai": return hasOpenaiKey
        case "openrouter": return hasOpenrouterKey
        case "gemini": return hasGeminiKey
        case "fireworks": return hasFireworksKey
        case "together": return hasTogetherKey
        case "custom": return hasCustomKey
        // Anthropic doesn't have STT scope (no Anthropic STT endpoint).
        default: return false
        }
    }

    /// Does the keystore hold a key for `providerId` under ANY scope
    /// (STT, LLM, or Recap)? Drives the "Connected" pill on the
    /// Providers and keys page. Mirror of the Win
    /// `SettingsViewModel.HasAnyKeyForProvider` method.
    func hasAnyKeyForProvider(_ providerId: String) -> Bool {
        let v = providerId.lowercased()
        if llmKeyByVendor[v] == true { return true }
        if recapKeyByVendor[v] == true { return true }
        switch v {
        case "groq": return hasGroqKey
        case "openai": return hasOpenaiKey
        case "openrouter": return hasOpenrouterKey
        case "gemini": return hasGeminiKey
        case "deepgram": return hasDeepgramKey
        case "fireworks": return hasFireworksKey
        case "together": return hasTogetherKey
        case "anthropic": return false  // no STT scope; LLM/Recap handled above
        case "custom": return hasCustomKey
        case "local": return true       // on-device, no key needed
        default: return false
        }
    }

    /// STT presets the user can actually use right now — those whose
    /// provider has a key saved in ANY scope (STT, LLM or Recap) plus
    /// `.custom` (always shown so the user can paste URL + key inline)
    /// and `.local` (no key needed). Mac diverges from Win here: Win
    /// shows the full catalog with an inline key field that materializes
    /// on selection; on Mac the user adds keys on the Providers page
    /// first, then comes here to route — so listing keyless providers
    /// just clutters the picker.
    func availableSttPresets() -> [SttPreset] {
        SttPreset.presets.filter { preset in
            if preset.provider == .custom { return true }
            return hasAnyKeyForProvider(preset.provider.rawValue)
        }
    }

    /// LLM presets the user can actually use right now. Always includes
    /// `custom` (URL + key inline) and `claude-code` (subscription via
    /// local `claude` CLI, no API key). Other presets accept ANY scope:
    /// a key saved for STT counts for LLM too — same provider key works
    /// on both audio and chat endpoints. Mac-only filter — see
    /// `availableSttPresets` for the rationale.
    func availableLlmPresets() -> [LlmPreset] {
        LlmPreset.presets.filter { preset in
            if preset.id == "custom" { return true }
            let vendor = llmProviderTag(forUrl: preset.apiUrl)
            // No API key required when the matching subscription CLI
            // is ready — the user can pick the provider and switch the
            // Authentication picker to Subscription. Without this,
            // a fresh install with Claude Code signed in but no
            // Anthropic API key would hide Anthropic from the dropdown
            // entirely, and the user would have nowhere to enable the
            // subscription path (the old `claude-code` synthetic
            // preset used to be that discovery affordance).
            if vendor == "anthropic" && claudeCodeReady { return true }
            if vendor == "openai" && codexReady { return true }
            return hasAnyKeyForProvider(vendor)
        }
    }

    /// Recap-model curated options the user can actually use right now.
    /// `local` always survives (bundled Gemma, no key); `custom` always
    /// survives (user-supplied id). `auto` survives only when at least
    /// one LLM-capable provider is connected — otherwise Auto would
    /// silently resolve to nothing and the recap call would fail. When
    /// Auto is filtered out, the picker shows a "Select model" prompt
    /// in its place (see `recapPickerNeedsSelectPrompt`). Mac-only
    /// filter — see `availableSttPresets` for the rationale.
    func availableRecapModels() -> [RecapModelOption] {
        RecapModelOption.curated.filter { opt in
            switch opt.provider {
            case .auto: return hasAnyLlmCapableKey
            case .local, .custom: return true
            case .anthropic:
                return recapKeyByVendor["anthropic"] == true
                    || llmKeyByVendor["anthropic"] == true
                    || claudeCodeReady
            case .gemini:
                return recapKeyByVendor["gemini"] == true
                    || llmKeyByVendor["gemini"] == true
                    || hasGeminiKey
            case .openai:
                // codexReady: the ChatGPT/Codex subscription is a keyless
                // CLI-routed path — without it a codex-only user never
                // sees the OpenAI recap models (Win parity:
                // FilterRecapModelPicker's `openai && codexReady`).
                return recapKeyByVendor["openai"] == true
                    || llmKeyByVendor["openai"] == true
                    || hasOpenaiKey
                    || codexReady
            }
        }
    }

    /// True when at least one provider that can run an LLM recap is
    /// connected (any scope, or Claude Code subscription). Drives the
    /// Auto / Select placeholder choice in `availableRecapModels` +
    /// `recapPickerNeedsSelectPrompt`.
    var hasAnyLlmCapableKey: Bool {
        if claudeCodeReady { return true }
        if codexReady { return true }
        if llmKeyByVendor.values.contains(true) { return true }
        if recapKeyByVendor.values.contains(true) { return true }
        return hasOpenaiKey || hasGeminiKey || hasGroqKey
            || hasOpenrouterKey || hasFireworksKey || hasTogetherKey
    }

    /// True when the recap picker should render a "Select model" prompt
    /// in place of the (filtered-out) Auto row. Triggered when the user
    /// has no LLM-capable provider AND no explicit override yet, so the
    /// picker doesn't silently leave them on a path that always fails.
    var recapPickerNeedsSelectPrompt: Bool {
        !hasAnyLlmCapableKey
            && recapModelOverride.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    private func sttProviderTag(forUrl url: String) -> String {
        if url.isEmpty { return "groq" }
        if url.contains("groq.com") { return "groq" }
        if url.contains("openai.com") { return "openai" }
        if url.contains("openrouter.ai") { return "openrouter" }
        if url.contains("googleapis.com") { return "gemini" }
        if url.contains("deepgram.com") { return "deepgram" }
        return "custom"
    }

    private func llmProviderTag(forUrl url: String) -> String {
        if url.isEmpty { return "groq" }
        let tag = ProviderTagging.providerTag(forUrl: url)
        if !tag.isEmpty { return tag }
        return "custom"
    }

    // MARK: - Call-detect nudge handlers

    /// Handle a `call_detected` event from the Rust core. Surfaces the
    /// nudge popup unless the user has disabled the feature.
    func onCallDetected(app: String?, sinceSecs: Int) {
        guard callDetectEnabled else { return }
        CallNudgeWindowController.shared.showDetected(app: app)
    }

    /// Handle a `call_ended` event. Hides any open nudge popup.
    func onCallEnded(app _: String?) {
        CallNudgeWindowController.shared.hide()
    }

    /// Handle a `meeting.stop_suggested` event. Pre-conditions are
    /// enforced Rust-side; we just paint the stop-mode popup.
    func onCallStopSuggested(app: String?, inactiveSecs _: Int) {
        guard callDetectEnabled else { return }
        CallNudgeWindowController.shared.showStopSuggestion(app: app)
    }

    /// Dispatch the user's response from the nudge popup. Beyond
    /// telling the Rust state machine, this also kicks the
    /// downstream side-effects: `record_now` opens the meeting
    /// window + starts a session; `stop_and_recap` runs the same
    /// stop+recap pipeline the pill uses.
    func callNudgeRespond(app: String?, response: String) {
        _ = DimmyCore.shared.callSignalResponse(appId: app, response: response)
        switch response {
        case "record_now":
            // Idempotency guard — mirror of Win's
            // `StartMeetingFromCallDetect`. If a meeting is already
            // active (e.g. a re-emit of the same nudge, or the user
            // had a meeting going from the home tile and tapped
            // Record on the call popup), don't try to start a second
            // one — just surface the existing window.
            if DimmyCore.shared.meetingIsActive {
                AppDelegate.shared?.openMeetingWindow()
                break
            }
            AppDelegate.shared?.openMeetingWindow()
            // openMeetingWindow shows + reattaches; the Meeting VM is
            // owned by the controller so we drive .start() through it.
            // Call-detect-started meetings default the recap flag to
            // true — the popup choice was "record this call", which
            // implies the user wants the recap too. Win parity:
            // MeetingGenerateRecap = true for call-detect-started.
            MeetingWindowController.shared.viewModel.generateRecap = true
            MeetingWindowController.shared.viewModel.start()
            // Bind the detected call as this meeting's origin so the
            // deterministic "call ended" path can watch it (Mac mirror of
            // Win MarkMeetingOriginFromCurrentSession). Falls back to the
            // silence heuristic if no origin pid was resolvable.
            CallDetectionManager.shared.markMeetingOrigin()
        case "stop_and_recap":
            PillWindowController.stopMeetingFromPill(appState: self)
        case "never":
            // Rust state machine already added the app to its in-memory
            // exclusion set + persisted via save_config_file. Mirror in
            // AppState so the Settings list refreshes without reloading
            // config from disk.
            if let app, !app.isEmpty,
               !callDetectExcludedApps.contains(app.lowercased()) {
                callDetectExcludedApps.append(app.lowercased())
            }
        default:
            break
        }
    }

    /// Remove an app from the exclusion list — Mac mirror of the
    /// Win settings "remove" button. Pushes the updated list back to
    /// Rust so the state machine stops suppressing nudges for it.
    func removeCallDetectExclusion(_ app: String) {
        let key = app.lowercased()
        callDetectExcludedApps.removeAll { $0 == key }
        DimmyCore.shared.setConfig(toRustConfig())
    }

    /// Build a config dictionary for sending to Rust via FFI.
    ///
    /// `includeNotion = false` (default) **omits** the
    /// `notion_target_id`/`_kind`/`_title` fields so a generic
    /// Settings save never wipes a valid Notion destination from
    /// disk. The Rust dispatcher treats an empty `notion_target_id`
    /// as a clear signal — if the AppState ever holds `""` (race,
    /// partial load, schema migration), the unguarded save would
    /// turn a "set" destination into "cleared" on every flush.
    /// Burned 2026-05-13 when meeting recap kept failing with
    /// "destination not configured" right after a Settings save.
    ///
    /// To explicitly set/clear the Notion destination the Notion
    /// picker / Disconnect path must call `toRustConfig(includeNotion: true)`.
    ///
    /// Same gating now applies to LLM + Recap identity fields
    /// (added 2026-05-15 after `llm_api_url` was silently wiped on
    /// the user's machine — same bug pattern as Notion, found
    /// during meeting-recap triage). `includeLlm` / `includeRecap`
    /// must be passed by the LLM/Recap-page save sites; everything
    /// else (theme toggle, hotkey change, etc.) omits those fields
    /// so the Rust core preserves the on-disk state.
    func toRustConfig(
        includeNotion: Bool = false,
        includeLlm: Bool = false,
        includeRecap: Bool = false
    ) -> [String: Any] {
        var config: [String: Any] = [
            "api_url": apiUrl,
            "api_model": apiModel,
            "language": Self.languageCode(for: selectedLanguage),
            "prompt": prompt,
            "shortcut_mode": preferredMode == .pushToTalk ? "hold" : "toggle",
            "llm_style": llmStyle,
            "llm_tone": llmTone,
            "llm_custom_prompt": llmCustomPrompt,
            "llm_translate_to": llmTranslateTo,
            "llm_log_enabled": llmLogEnabled,
            "stt_mode": sttMode,
            "local_model": localModel,
            "local_stt_backend": localSttBackend,
            "live_captions_enabled": liveCaptionsEnabled,
            "filler_removal_enabled": fillerRemovalEnabled,
            "call_detect_enabled": callDetectEnabled,
            "call_detect_excluded_apps": callDetectExcludedApps,
            "preprocessing_enabled": preprocessingEnabled,
            "chunk_streaming_enabled": chunkStreamingEnabled,
            "audio_debug_enabled": audioDebugEnabled,
            "input_gain": Double(inputGain),
            "border_style": borderStyle,
            "waveform_style": waveformStyle,
            "overlay_position": overlayPosition,
            "keep_in_clipboard": keepInClipboard,
            "use_keyring": false,  // Always local encrypted file
            "save_audio_in_history": saveAudioInHistory,
            "history_audio_keep_days": Int(historyAudioKeepDays),
            "history_audio_max_mb": Int(historyAudioMaxMb),
            "auto_recap_threshold_secs": Int(autoRecapThresholdSecs),
            // notion_target_{id,kind,title} are deliberately gated
            // behind `includeNotion` — see toRustConfig docstring above.
            // notion_auto_send IS safe to round-trip (bool toggle,
            // not a load-bearing identifier).
            "notion_auto_send": notionAutoSend,
            "user_dict": userDictWords,
            "app_rules": appRules.map { $0.toDict() },
        ]
        if includeLlm {
            // LLM provider identity — wiping these silently kills
            // the user's LLM setup. The Settings → LLM page is the
            // single legitimate writer; other save sites must omit
            // them so the Rust core preserves disk state.
            config["llm_api_url"] = llmApiUrl
            config["llm_api_model"] = llmApiModel
            config["llm_use_same_key"] = llmUseSameKey
            config["llm_auth_method"] = llmAuthMethod
            config["llm_mode"] = llmMode
            config["local_llm_model"] = localLlmModel
            // `llm_enabled` is derived from `llm_style != "off"`
            // upstream; keep it gated with the rest of LLM identity.
            config["llm_enabled"] = llmEnabled
        }
        if includeRecap {
            // Recap section in Settings → Output owns these fields.
            config["recap_auth_method"] = recapAuthMethod
            config["recap_model_override"] = recapModelOverride
            config["recap_use_same_key"] = recapUseSameKey
        }
        if includeNotion {
            config["notion_target_id"] = notionTargetId
            config["notion_target_kind"] = notionTargetKind
            config["notion_target_title"] = notionTargetTitle
        }
        if let dev = selectedDevice {
            config["selected_device"] = dev
        }
        // Identity-class: emit ONLY when non-empty so a transient/full save
        // never wipes the user's custom dir. The explicit Reset button
        // sends a targeted {"meeting_storage_path":""} to reset to default.
        if !meetingStoragePath.isEmpty {
            config["meeting_storage_path"] = meetingStoragePath
        }
        return config
    }

    // MARK: - Language helpers

    nonisolated static func languageCode(for display: String) -> String {
        languageMap.first(where: { $0.display == display })?.code ?? ""
    }

    nonisolated static func displayLanguage(for code: String) -> String {
        languageMap.first(where: { $0.code == code })?.display ?? "Auto-detect"
    }
}

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
    func resetTccEntries(services: [String]) {
        let bundleId = Bundle.main.bundleIdentifier ?? "com.dimmy.app"
        for service in services {
            let process = Process()
            process.launchPath = "/usr/bin/tccutil"
            process.arguments = ["reset", service, bundleId]
            do {
                try process.run()
                process.waitUntilExit()
            } catch {
                NSLog("[PermissionsManager] tccutil reset %@ failed: %@", service, error.localizedDescription)
            }
        }
        refresh()
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

    static let presets: [SttPreset] = [
        SttPreset(id: "groq-whisper-turbo", displayName: "Groq \u{2014} whisper-large-v3-turbo (free)", provider: .groq, apiUrl: "https://api.groq.com/openai/v1/audio/transcriptions", model: "whisper-large-v3-turbo"),
        SttPreset(id: "groq-whisper-v3", displayName: "Groq \u{2014} whisper-large-v3 (free)", provider: .groq, apiUrl: "https://api.groq.com/openai/v1/audio/transcriptions", model: "whisper-large-v3"),
        SttPreset(id: "groq-distil-en", displayName: "Groq \u{2014} distil-whisper-en (free)", provider: .groq, apiUrl: "https://api.groq.com/openai/v1/audio/transcriptions", model: "distil-whisper-large-v3-en"),
        SttPreset(id: "openai-whisper1", displayName: "OpenAI \u{2014} whisper-1", provider: .openai, apiUrl: "https://api.openai.com/v1/audio/transcriptions", model: "whisper-1"),
        SttPreset(id: "openai-4o-transcribe", displayName: "OpenAI \u{2014} gpt-4o-transcribe", provider: .openai, apiUrl: "https://api.openai.com/v1/audio/transcriptions", model: "gpt-4o-transcribe"),
        SttPreset(id: "openai-4o-mini-transcribe", displayName: "OpenAI \u{2014} gpt-4o-mini-transcribe", provider: .openai, apiUrl: "https://api.openai.com/v1/audio/transcriptions", model: "gpt-4o-mini-transcribe"),
        SttPreset(id: "deepgram-nova3", displayName: "Deepgram \u{2014} nova-3", provider: .deepgram, apiUrl: "https://api.deepgram.com/v1/listen", model: "nova-3"),
        SttPreset(id: "deepgram-nova2", displayName: "Deepgram \u{2014} nova-2", provider: .deepgram, apiUrl: "https://api.deepgram.com/v1/listen", model: "nova-2"),
        SttPreset(id: "gemini-flash", displayName: "Gemini \u{2014} gemini-2.5-flash (free)", provider: .gemini, apiUrl: "https://generativelanguage.googleapis.com/v1beta/models", model: "gemini-2.5-flash"),
        SttPreset(id: "gemini-pro", displayName: "Gemini \u{2014} gemini-2.5-pro (free)", provider: .gemini, apiUrl: "https://generativelanguage.googleapis.com/v1beta/models", model: "gemini-2.5-pro"),
        SttPreset(id: "custom", displayName: "Custom endpoint", provider: .custom, apiUrl: "", model: ""),
    ]

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

    static let presets: [LlmPreset] = [
        LlmPreset(id: "groq-llama70b", displayName: "Groq \u{2014} llama-3.3-70b (free)", apiUrl: "https://api.groq.com/openai/v1/chat/completions", model: "llama-3.3-70b-versatile"),
        LlmPreset(id: "openai-4o-mini", displayName: "OpenAI \u{2014} gpt-4o-mini", apiUrl: "https://api.openai.com/v1/chat/completions", model: "gpt-4o-mini"),
        LlmPreset(id: "openrouter-llama70b", displayName: "OpenRouter \u{2014} llama-3.3-70b (free)", apiUrl: "https://openrouter.ai/api/v1/chat/completions", model: "meta-llama/llama-3.3-70b-instruct:free"),
        LlmPreset(id: "openrouter-deepseek", displayName: "OpenRouter \u{2014} DeepSeek R1 (free)", apiUrl: "https://openrouter.ai/api/v1/chat/completions", model: "deepseek/deepseek-r1:free"),
        LlmPreset(id: "gemini-flash", displayName: "Gemini \u{2014} gemini-2.5-flash (free)", apiUrl: "https://generativelanguage.googleapis.com/v1beta/models", model: "gemini-2.5-flash"),
        LlmPreset(id: "anthropic-haiku", displayName: "Anthropic \u{2014} claude-haiku-4.5", apiUrl: "https://api.anthropic.com/v1/messages", model: "claude-haiku-4.5-20250315"),
        LlmPreset(id: "anthropic-sonnet", displayName: "Anthropic \u{2014} claude-sonnet-4", apiUrl: "https://api.anthropic.com/v1/messages", model: "claude-sonnet-4-20250514"),
        LlmPreset(id: "custom", displayName: "Custom endpoint", apiUrl: "", model: ""),
    ]

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
    case custom

    var id: String { rawValue }

    var displayName: String {
        switch self {
        case .groq: return "Groq"
        case .openai: return "OpenAI"
        case .deepgram: return "Deepgram"
        case .gemini: return "Gemini"
        case .custom: return "Custom"
        }
    }

    var defaultApiUrl: String {
        switch self {
        case .groq: return "https://api.groq.com/openai/v1/audio/transcriptions"
        case .openai: return "https://api.openai.com/v1/audio/transcriptions"
        case .deepgram: return "https://api.deepgram.com/v1/listen"
        case .gemini: return "https://generativelanguage.googleapis.com/v1beta/models"
        case .custom: return ""
        }
    }

    var defaultModel: String {
        switch self {
        case .groq: return "whisper-large-v3-turbo"
        case .openai: return "whisper-1"
        case .deepgram: return "nova-2"
        case .gemini: return "gemini-2.0-flash"
        case .custom: return ""
        }
    }

    var models: [String] {
        switch self {
        case .groq: return ["whisper-large-v3-turbo", "whisper-large-v3", "distil-whisper-large-v3-en"]
        case .openai: return ["whisper-1", "gpt-4o-transcribe", "gpt-4o-mini-transcribe"]
        case .deepgram: return ["nova-2", "nova-3"]
        case .gemini: return ["gemini-2.0-flash", "gemini-2.0-flash-lite"]
        case .custom: return []
        }
    }

    static func from(url: String) -> SttProvider {
        if url.contains("groq.com") { return .groq }
        if url.contains("openai.com") { return .openai }
        if url.contains("deepgram.com") { return .deepgram }
        if url.contains("googleapis.com") { return .gemini }
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

    // MARK: - LLM Mode (local vs cloud)

    @Published var llmMode: String = "cloud"  // "local" or "cloud"
    @Published var localLlmModel: String = "gemma-4-E2B-it-Q4_K_M.gguf"
    @Published var llmModelDownloadProgress: Double = 0.0
    @Published var isDownloadingLlmModel: Bool = false

    // MARK: - STT Config (synced with Rust via FFI)

    @Published var sttProvider: SttProvider = .groq
    @Published var apiUrl: String = "https://api.groq.com/openai/v1/audio/transcriptions"
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
    @Published var llmApiUrl: String = ""
    @Published var llmApiModel: String = ""
    @Published var llmUseSameKey: Bool = true
    @Published var hasLlmKey: Bool = false
    @Published var llmLogEnabled: Bool = false

    // MARK: - Audio Config

    @Published var preprocessingEnabled: Bool = true
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

    // MARK: - Stats

    @Published var statsTotalWords: UInt64 = 0
    @Published var statsTotalSpeakingSecs: Double = 0.0

    // MARK: - Per-provider key flags

    @Published var hasGroqKey: Bool = false
    @Published var hasOpenaiKey: Bool = false
    @Published var hasGeminiKey: Bool = false
    @Published var hasDeepgramKey: Bool = false
    @Published var hasCustomKey: Bool = false

    // MARK: - Language list (display name → language code for Rust)

    static let languageMap: [(display: String, code: String)] = [
        ("Auto Detect", ""),
        ("Italiano", "it"),
        ("English", "en"),
        ("Español", "es"),
        ("Français", "fr"),
        ("Deutsch", "de"),
        ("Português", "pt"),
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
            // without digging into Settings. Users can still pick "Auto Detect" explicitly later.
            let effectiveLang = (lang.isEmpty && !isOnboardingComplete)
                ? Self.systemPreferredLanguageCode()
                : lang
            selectedLanguage = Self.displayLanguage(for: effectiveLang)
        }
        if let p = config["prompt"] as? String { prompt = p }
        if let hk = config["has_key"] as? Bool { hasKey = hk }
        if let dev = config["selected_device"] as? String { selectedDevice = dev }
        if let devs = config["devices"] as? [String] { devices = devs }

        // Local STT
        if let v = config["stt_mode"] as? String { sttMode = v }
        if let v = config["local_model"] as? String { localModel = v }
        if let v = config["filler_removal_enabled"] as? Bool { fillerRemovalEnabled = v }

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
        if let hlk = config["has_llm_key"] as? Bool { hasLlmKey = hlk }
        if let ll = config["llm_log_enabled"] as? Bool { llmLogEnabled = ll }

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

        // Per-provider key flags
        if let v = config["has_groq_key"] as? Bool { hasGroqKey = v }
        if let v = config["has_openai_key"] as? Bool { hasOpenaiKey = v }
        if let v = config["has_gemini_key"] as? Bool { hasGeminiKey = v }
        if let v = config["has_deepgram_key"] as? Bool { hasDeepgramKey = v }
        if let v = config["has_custom_key"] as? Bool { hasCustomKey = v }
    }

    /// Build a config dictionary for sending to Rust via FFI.
    func toRustConfig() -> [String: Any] {
        var config: [String: Any] = [
            "api_url": apiUrl,
            "api_model": apiModel,
            "language": Self.languageCode(for: selectedLanguage),
            "prompt": prompt,
            "shortcut_mode": preferredMode == .pushToTalk ? "hold" : "toggle",
            "llm_enabled": llmEnabled,
            "llm_style": llmStyle,
            "llm_tone": llmTone,
            "llm_custom_prompt": llmCustomPrompt,
            "llm_translate_to": llmTranslateTo,
            "llm_api_url": llmApiUrl,
            "llm_api_model": llmApiModel,
            "llm_use_same_key": llmUseSameKey,
            "llm_log_enabled": llmLogEnabled,
            "stt_mode": sttMode,
            "local_model": localModel,
            "filler_removal_enabled": fillerRemovalEnabled,
            "llm_mode": llmMode,
            "local_llm_model": localLlmModel,
            "preprocessing_enabled": preprocessingEnabled,
            "chunk_streaming_enabled": chunkStreamingEnabled,
            "audio_debug_enabled": audioDebugEnabled,
            "input_gain": Double(inputGain),
            "border_style": borderStyle,
            "waveform_style": waveformStyle,
            "overlay_position": overlayPosition,
            "keep_in_clipboard": keepInClipboard,
            "use_keyring": false,  // Always local encrypted file
        ]
        if let dev = selectedDevice {
            config["selected_device"] = dev
        }
        return config
    }

    // MARK: - Language helpers

    nonisolated static func languageCode(for display: String) -> String {
        languageMap.first(where: { $0.display == display })?.code ?? ""
    }

    nonisolated static func displayLanguage(for code: String) -> String {
        languageMap.first(where: { $0.code == code })?.display ?? "Auto Detect"
    }
}

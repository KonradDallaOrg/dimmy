import AppKit
import Combine

/// Status-bar (menu-bar) controller. Owns the NSStatusItem and the
/// NSMenu that appears when the user clicks the icon in the macOS
/// menu bar (top-right of the screen).
///
/// We use a native `NSMenu` (not a SwiftUI popover) so the user gets:
///   - real macOS submenus for "Translate to" and "Style" — pick a value
///     in two clicks without opening Settings or scrolling on the pill,
///   - keyboard navigation, Voice Control, accessibility for free,
///   - an idiomatic "this app lives in the menu bar" UX.
///
/// `NSMenuDelegate.menuNeedsUpdate` rebuilds the menu on every open, so
/// the checkmarks (current Translate-to / Style / state) are always
/// fresh — no need to wire the entire menu to Combine publishers.
@MainActor
final class StatusBarController: NSObject, NSMenuDelegate {
    private var statusItem: NSStatusItem?
    private var appState: AppState
    private var cancellables = Set<AnyCancellable>()

    /// Translate-target options surfaced in the menu. Mirrors
    /// `TranslateTargets` in Windows' `SettingsViewModel.cs` so the two
    /// platforms stay in sync. The empty-string code means "no
    /// translation" (transcript stays in source language) — matches the
    /// Rust core convention.
    private static let translateTargets: [(code: String, label: String)] = [
        ("", "No translation"),
        ("it", "Italiano"),
        ("en", "English"),
        ("es", "Español"),
        ("fr", "Français"),
        ("de", "Deutsch"),
        ("pt", "Português"),
    ]

    init(appState: AppState) {
        self.appState = appState
        super.init()
        setupStatusItem()
        observeState()
    }

    private func setupStatusItem() {
        statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)

        guard let button = statusItem?.button else { return }
        let config = NSImage.SymbolConfiguration(pointSize: 16, weight: .regular)
        button.image = NSImage(systemSymbolName: "waveform.circle", accessibilityDescription: "Dimmy")?
            .withSymbolConfiguration(config)
        button.image?.isTemplate = true

        // Assigning .menu (instead of .action) makes the status item
        // open the menu on click — and the menuNeedsUpdate delegate
        // method below rebuilds it dynamically before every open.
        let menu = NSMenu()
        menu.delegate = self
        statusItem?.menu = menu
    }

    private func observeState() {
        appState.$recordingState
            .receive(on: DispatchQueue.main)
            .sink { [weak self] state in
                self?.updateIcon(for: state, hotkey: self?.appState.hotkeyStatus ?? .uninstalled)
            }
            .store(in: &cancellables)

        appState.$hotkeyStatus
            .receive(on: DispatchQueue.main)
            .sink { [weak self] status in
                self?.updateIcon(for: self?.appState.recordingState ?? .idle, hotkey: status)
            }
            .store(in: &cancellables)
    }

    private func updateIcon(for state: RecordingState, hotkey: HotkeyStatus) {
        guard let button = statusItem?.button else { return }
        let size = NSImage.SymbolConfiguration(pointSize: 16, weight: .regular)

        // Hotkey health overrides idle icon so users see the problem at a glance.
        if case .idle = state, hotkey != .installed {
            let warn = size.applying(NSImage.SymbolConfiguration(paletteColors: [.systemOrange]))
            button.image = NSImage(systemSymbolName: "exclamationmark.triangle.fill",
                                   accessibilityDescription: "Dimmy - Hotkey disabled")?
                .withSymbolConfiguration(warn)
            button.image?.isTemplate = false
            button.toolTip = Self.tooltip(for: hotkey)
            return
        }

        button.toolTip = nil

        switch state {
        case .idle:
            button.image = NSImage(systemSymbolName: "waveform.circle", accessibilityDescription: "Dimmy - Ready")?
                .withSymbolConfiguration(size)
            button.image?.isTemplate = true
        case .recording:
            let config = size.applying(NSImage.SymbolConfiguration(paletteColors: [.systemRed]))
            button.image = NSImage(systemSymbolName: "waveform.circle.fill", accessibilityDescription: "Dimmy - Recording")?
                .withSymbolConfiguration(config)
            button.image?.isTemplate = false
        case .transcribing:
            let config = size.applying(NSImage.SymbolConfiguration(paletteColors: [.systemBlue]))
            button.image = NSImage(systemSymbolName: "ellipsis.circle.fill", accessibilityDescription: "Dimmy - Transcribing")?
                .withSymbolConfiguration(config)
            button.image?.isTemplate = false
        case .processing:
            let config = size.applying(NSImage.SymbolConfiguration(paletteColors: [.systemPurple]))
            button.image = NSImage(systemSymbolName: "sparkles", accessibilityDescription: "Dimmy - Processing")?
                .withSymbolConfiguration(config)
            button.image?.isTemplate = false
        case .completing:
            let config = size.applying(NSImage.SymbolConfiguration(paletteColors: [.systemGreen]))
            button.image = NSImage(systemSymbolName: "checkmark.circle.fill", accessibilityDescription: "Dimmy - Done")?
                .withSymbolConfiguration(config)
            button.image?.isTemplate = false
        }
    }

    private static func tooltip(for hotkey: HotkeyStatus) -> String {
        switch hotkey {
        case .installed: return ""
        case .uninstalled: return "Dimmy: hotkey not yet initialized"
        case .accessibilityMissing: return "Dimmy: shortcut disabled — grant Accessibility in System Settings"
        case .tapFailed(let reason): return "Dimmy: shortcut disabled (\(reason))"
        }
    }

    // MARK: - NSMenuDelegate

    /// Rebuild the menu just before it appears so checkmarks and
    /// disabled-row labels reflect current `appState`.
    func menuNeedsUpdate(_ menu: NSMenu) {
        menu.removeAllItems()

        // Status row (disabled label).
        let statusItem = NSMenuItem(title: statusText, action: nil, keyEquivalent: "")
        statusItem.isEnabled = false
        menu.addItem(statusItem)
        menu.addItem(NSMenuItem.separator())

        // Native input language (read-only — STT setting, lives in
        // Settings → Voice). Distinct from "Translate to" which is the
        // LLM output target.
        let nativeLang = appState.selectedLanguage.isEmpty ? "(auto)" : appState.selectedLanguage
        let nativeItem = NSMenuItem(title: "Native: \(nativeLang)", action: nil, keyEquivalent: "")
        nativeItem.isEnabled = false
        menu.addItem(nativeItem)

        // Translate-to submenu.
        let translateLabel = appState.llmTranslateTo.isEmpty || appState.llmTranslateTo == "none"
            ? "(none)"
            : appState.llmTranslateTo
        let translateItem = NSMenuItem(title: "Translate to: \(translateLabel)", action: nil, keyEquivalent: "")
        translateItem.submenu = buildTranslateToSubmenu()
        menu.addItem(translateItem)

        // Style submenu.
        let styleLabel = appState.llmStyleEnum.displayName
        let styleItem = NSMenuItem(title: "Style: \(styleLabel)", action: nil, keyEquivalent: "")
        styleItem.submenu = buildStyleSubmenu()
        menu.addItem(styleItem)

        // Shortcut (read-only).
        let shortcutItem = NSMenuItem(title: "Shortcut: \(appState.shortcut.displayString)",
                                      action: nil, keyEquivalent: "")
        shortcutItem.isEnabled = false
        menu.addItem(shortcutItem)

        menu.addItem(NSMenuItem.separator())

        // Actions.
        let settingsItem = NSMenuItem(title: "Settings…", action: #selector(openSettings), keyEquivalent: ",")
        settingsItem.target = self
        menu.addItem(settingsItem)

        let quitItem = NSMenuItem(title: "Quit Dimmy", action: #selector(quitApp), keyEquivalent: "q")
        quitItem.target = self
        menu.addItem(quitItem)
    }

    private func buildTranslateToSubmenu() -> NSMenu {
        let submenu = NSMenu()
        // Treat both legacy "none" and empty as the no-translation state.
        let current = (appState.llmTranslateTo == "none") ? "" : appState.llmTranslateTo
        for target in Self.translateTargets {
            let item = NSMenuItem(title: target.label,
                                  action: #selector(handleTranslateTo(_:)),
                                  keyEquivalent: "")
            item.target = self
            item.representedObject = target.code
            item.state = (target.code == current) ? .on : .off
            submenu.addItem(item)
        }
        return submenu
    }

    private func buildStyleSubmenu() -> NSMenu {
        let submenu = NSMenu()
        for style in LlmStyle.allCases {
            let item = NSMenuItem(title: style.displayName,
                                  action: #selector(handleStyle(_:)),
                                  keyEquivalent: "")
            item.target = self
            item.representedObject = style.rawValue
            item.state = (style == appState.llmStyleEnum) ? .on : .off
            submenu.addItem(item)
        }
        return submenu
    }

    @objc private func handleTranslateTo(_ sender: NSMenuItem) {
        guard let code = sender.representedObject as? String else { return }
        appState.llmTranslateTo = code
        DimmyCore.shared.setConfig(appState.toRustConfig())
    }

    @objc private func handleStyle(_ sender: NSMenuItem) {
        guard let raw = sender.representedObject as? String else { return }
        appState.llmStyle = raw
        DimmyCore.shared.setConfig(appState.toRustConfig())
    }

    @objc private func openSettings() {
        AppDelegate.shared?.openSettings()
    }

    @objc private func quitApp() {
        NSApplication.shared.terminate(nil)
    }

    private var statusText: String {
        switch appState.recordingState {
        case .idle: return "● Ready"
        case .recording(.pushToTalk): return "● Recording (hold)…"
        case .recording(.toggle): return "● Recording…"
        case .transcribing: return "● Transcribing…"
        case .processing: return "● Processing…"
        case .completing: return "● Done"
        }
    }
}

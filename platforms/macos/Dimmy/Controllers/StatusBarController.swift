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
        if appState.showInMenuBar {
            setupStatusItem()
        }
        observeState()
    }

    private func setupStatusItem() {
        guard statusItem == nil else { return }
        statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)

        guard let button = statusItem?.button else { return }
        button.image = Self.menuBarImage(symbolName: "waveform.circle",
                                         accessibility: "Dimmy",
                                         isTemplate: true)

        // Assigning .menu (instead of .action) makes the status item
        // open the menu on click — and the menuNeedsUpdate delegate
        // method below rebuilds it dynamically before every open.
        let menu = NSMenu()
        menu.delegate = self
        statusItem?.menu = menu

        // Refresh icon to reflect current state.
        updateIcon(for: appState.recordingState, hotkey: appState.hotkeyStatus)
    }

    private func teardownStatusItem() {
        guard let item = statusItem else { return }
        NSStatusBar.system.removeStatusItem(item)
        statusItem = nil
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

        appState.$showInMenuBar
            .receive(on: DispatchQueue.main)
            .sink { [weak self] visible in
                guard let self else { return }
                if visible {
                    self.setupStatusItem()
                } else {
                    self.teardownStatusItem()
                }
            }
            .store(in: &cancellables)
    }

    private func updateIcon(for state: RecordingState, hotkey: HotkeyStatus) {
        guard let button = statusItem?.button else { return }

        // Hotkey health overlays a small yellow badge on top of the regular
        // Dimmy icon — keeps the brand recognisable in the menubar instead
        // of replacing it with a generic warning triangle.
        if case .idle = state, hotkey != .installed {
            button.image = Self.makeWarningBadgedIcon()
            button.toolTip = Self.tooltip(for: hotkey)
            return
        }

        button.toolTip = nil

        switch state {
        case .idle:
            button.image = Self.menuBarImage(symbolName: "waveform.circle",
                                             accessibility: "Dimmy - Ready",
                                             isTemplate: true)
        case .recording:
            button.image = Self.menuBarImage(symbolName: "waveform.circle.fill",
                                             accessibility: "Dimmy - Recording",
                                             paletteColor: .systemRed)
        case .transcribing:
            button.image = Self.menuBarImage(symbolName: "ellipsis.circle.fill",
                                             accessibility: "Dimmy - Transcribing",
                                             paletteColor: .systemBlue)
        case .processing:
            button.image = Self.menuBarImage(symbolName: "sparkles",
                                             accessibility: "Dimmy - Processing",
                                             paletteColor: .systemPurple)
        case .completing:
            button.image = Self.menuBarImage(symbolName: "checkmark.circle.fill",
                                             accessibility: "Dimmy - Done",
                                             paletteColor: .systemGreen)
        }
    }

    /// Build an NSImage suitable for `NSStatusItem.button.image`.
    ///
    /// Why the explicit `size` pin: SF Symbol images returned by
    /// `withSymbolConfiguration(pointSize:)` come with an intrinsic
    /// size derived from the symbol's vector bounds at the requested
    /// point size — and that intrinsic value can change when the
    /// backing scale factor changes (notched MacBook display ↔
    /// external monitor with different DPI). The status bar button
    /// then renders the image at the new intrinsic size, which can
    /// overflow the menubar's content area and get clipped.
    ///
    /// Pinning `image.size = 18×18 pt` keeps the LOGICAL display size
    /// deterministic — the system still picks the correct pixel-density
    /// representation from the underlying vector, but never grows or
    /// shrinks the layout box. 18 pt matches Apple's HIG recommendation
    /// for menubar extras.
    ///
    /// Burned 2026-05-13: status icon visibly oversized + cropped after
    /// dragging the laptop between built-in display and 4K external.
    private static let menuBarIconSize = NSSize(width: 18, height: 18)
    private static let menuBarSymbolPointSize: CGFloat = 16

    private static func menuBarImage(symbolName: String,
                                     accessibility: String,
                                     isTemplate: Bool = false,
                                     paletteColor: NSColor? = nil) -> NSImage? {
        var config = NSImage.SymbolConfiguration(pointSize: menuBarSymbolPointSize,
                                                 weight: .regular)
        if let palette = paletteColor {
            config = config.applying(NSImage.SymbolConfiguration(paletteColors: [palette]))
        }
        guard let image = NSImage(systemSymbolName: symbolName,
                                  accessibilityDescription: accessibility)?
                .withSymbolConfiguration(config)
        else { return nil }
        image.size = menuBarIconSize
        image.isTemplate = isTemplate
        return image
    }

    /// Compose the steady Dimmy waveform icon with a small yellow
    /// exclamation badge at the bottom-right. The base symbol is tinted
    /// with `NSColor.labelColor` so it reads in both light and dark
    /// menubars; the badge keeps its yellow palette colour.
    /// Returned as non-template (it's deliberately multi-colour).
    private static func makeWarningBadgedIcon() -> NSImage? {
        let basePalette = NSImage.SymbolConfiguration(pointSize: menuBarSymbolPointSize, weight: .regular)
            .applying(NSImage.SymbolConfiguration(paletteColors: [NSColor.labelColor]))
        guard let base = NSImage(systemSymbolName: "waveform.circle",
                                 accessibilityDescription: "Dimmy - Hotkey disabled")?
                .withSymbolConfiguration(basePalette) else { return nil }
        // Pin the base symbol's logical size to the same 18×18 box the
        // composite uses so its intrinsic vector bounds can never push
        // the drawing outside the canvas when the backing scale changes.
        base.size = menuBarIconSize

        let badgePalette = NSImage.SymbolConfiguration(pointSize: 9, weight: .bold)
            .applying(NSImage.SymbolConfiguration(paletteColors: [NSColor.systemYellow]))
        let badge = NSImage(systemSymbolName: "exclamationmark.circle.fill",
                            accessibilityDescription: nil)?
            .withSymbolConfiguration(badgePalette)
        badge?.size = NSSize(width: 10, height: 10)

        let composed = NSImage(size: menuBarIconSize, flipped: false) { _ in
            base.draw(in: NSRect(origin: .zero, size: menuBarIconSize),
                      from: .zero, operation: .sourceOver, fraction: 1.0)

            if let badge {
                let badgeSize = badge.size
                let origin = NSPoint(x: menuBarIconSize.width - badgeSize.width,
                                     y: 0)
                badge.draw(in: NSRect(origin: origin, size: badgeSize),
                           from: .zero, operation: .sourceOver, fraction: 1.0)
            }
            return true
        }
        // Marked non-template because the yellow badge palette is
        // intentional — letting macOS auto-tint would erase it.
        composed.isTemplate = false
        return composed
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

        // Status row (disabled label) — leading "●" is colored to match
        // the state (mirrors the colors used on the menu-bar icon).
        let statusItem = NSMenuItem(title: "", action: nil, keyEquivalent: "")
        statusItem.attributedTitle = makeStatusAttributedTitle()
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

        // Show/hide pill toggle. Title flips with state — same wording as
        // the Dock menu, kept distinct from system "Hide Dimmy" by saying
        // "Pill Overlay".
        let pillTitle = appState.pillVisible ? "Hide Pill Overlay" : "Show Pill Overlay"
        let pillItem = NSMenuItem(title: pillTitle,
                                  action: #selector(togglePillVisibility),
                                  keyEquivalent: "")
        pillItem.target = self
        menu.addItem(pillItem)

        menu.addItem(NSMenuItem.separator())

        // UI-driven recording toggle. Bypasses the global hotkey path so
        // users without Accessibility (e.g. fresh Debug builds whose
        // signature voided the grant) can still trigger a recording.
        // Mic permission is still required — the system prompt fires on
        // first invocation. Disabled while the core hasn't initialised.
        let recordTitle: String
        if appState.isRecording {
            recordTitle = "Stop Recording"
        } else {
            recordTitle = "Start Recording…"
        }
        let recordItem = NSMenuItem(title: recordTitle,
                                    action: #selector(toggleRecordingFromMenu),
                                    keyEquivalent: "r")
        recordItem.target = self
        recordItem.isEnabled = DimmyCore.shared.isInitialized
        menu.addItem(recordItem)

        // Open Meeting… — Phase 4 entry point. Distinct from the
        // dictation hotkey: starts a long-form recording with live
        // transcript + post-stop LLM recap. Mirrors the Win tray entry.
        let meetingItem = NSMenuItem(title: "Open Meeting…",
                                     action: #selector(openMeeting),
                                     keyEquivalent: "")
        meetingItem.target = self
        menu.addItem(meetingItem)

        menu.addItem(NSMenuItem.separator())

        // Actions.
        let settingsItem = NSMenuItem(title: "Settings…", action: #selector(openSettings), keyEquivalent: ",")
        settingsItem.target = self
        menu.addItem(settingsItem)

        let quitItem = NSMenuItem(title: "Quit Dimmy", action: #selector(quitApp), keyEquivalent: "q")
        quitItem.target = self
        menu.addItem(quitItem)
    }

    @objc private func openMeeting() {
        AppDelegate.shared?.openMeetingWindow()
    }

    @objc private func toggleRecordingFromMenu() {
        // Lazy-init in case the user got here before mic permission ever
        // mattered. dimmy_init is idempotent.
        if !DimmyCore.shared.isInitialized {
            DispatchQueue.global(qos: .userInitiated).async {
                _ = DimmyCore.shared.initialize()
                DispatchQueue.main.async { HotkeyManager.shared.toggleRecordingFromUI() }
            }
            return
        }
        HotkeyManager.shared.toggleRecordingFromUI()
    }

    /// Public so AppDelegate can reuse it inside `applicationDockMenu(_:)`,
    /// keeping the menu-bar and Dock right-click menus in sync.
    func buildTranslateToSubmenu() -> NSMenu {
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

    func buildStyleSubmenu() -> NSMenu {
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

    @objc func togglePillVisibility() {
        hkLog("[StatusBar] togglePillVisibility — was=\(appState.pillVisible)")
        appState.pillVisible.toggle()
    }

    @objc private func openSettings() {
        AppDelegate.shared?.openSettings()
    }

    @objc private func quitApp() {
        NSApplication.shared.terminate(nil)
    }

    private var statusLabel: String {
        switch appState.recordingState {
        case .idle:
            return appState.hotkeyStatus == .installed ? "Ready" : "Hotkey disabled"
        case .recording(.pushToTalk): return "Recording (hold)…"
        case .recording(.toggle): return "Recording…"
        case .transcribing: return "Transcribing…"
        case .processing: return "Processing…"
        case .completing: return "Done"
        }
    }

    private var statusDotColor: NSColor {
        switch appState.recordingState {
        case .idle:
            return appState.hotkeyStatus == .installed ? .systemGreen : .systemYellow
        case .recording: return .systemRed
        case .transcribing: return .systemBlue
        case .processing: return .systemPurple
        case .completing: return .systemGreen
        }
    }

    private func makeStatusAttributedTitle() -> NSAttributedString {
        let result = NSMutableAttributedString()
        let dotFont = NSFont.systemFont(ofSize: NSFont.systemFontSize)
        let dot = NSAttributedString(
            string: "● ",
            attributes: [
                .foregroundColor: statusDotColor,
                .font: dotFont,
            ]
        )
        let label = NSAttributedString(
            string: statusLabel,
            attributes: [
                .foregroundColor: NSColor.labelColor,
                .font: dotFont,
            ]
        )
        result.append(dot)
        result.append(label)
        return result
    }
}

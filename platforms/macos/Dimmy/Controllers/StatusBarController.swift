import AppKit
import SwiftUI
import Combine

@MainActor
final class StatusBarController: NSObject {
    private var statusItem: NSStatusItem?
    private var popover: NSPopover?
    private var appState: AppState
    private var cancellables = Set<AnyCancellable>()

    init(appState: AppState) {
        self.appState = appState
        super.init()
        setupStatusItem()
        setupPopover()
        observeState()
    }

    private func setupStatusItem() {
        statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)

        guard let button = statusItem?.button else { return }
        let config = NSImage.SymbolConfiguration(pointSize: 16, weight: .regular)
        button.image = NSImage(systemSymbolName: "waveform.circle", accessibilityDescription: "Dimmy")?
            .withSymbolConfiguration(config)
        button.image?.isTemplate = true
        button.action = #selector(togglePopover)
        button.target = self
    }

    private func setupPopover() {
        popover = NSPopover()
        popover?.contentSize = NSSize(width: 220, height: 260)
        popover?.behavior = .transient
        popover?.animates = true
        popover?.contentViewController = NSHostingController(
            rootView: MenuBarPopover(appState: appState)
        )
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

        // Hotkey health overlays a small yellow badge on top of the regular
        // Dimmy icon — keeps the brand recognisable in the menubar instead
        // of replacing it with a generic warning triangle.
        if case .idle = state, hotkey != .installed {
            button.image = Self.makeWarningBadgedIcon()
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

    /// Compose the steady Dimmy waveform icon with a small yellow
    /// exclamation badge at the bottom-right. The base symbol is tinted
    /// with `NSColor.labelColor` so it reads in both light and dark
    /// menubars; the badge keeps its yellow palette colour.
    /// Returned as non-template (it's deliberately multi-colour).
    private static func makeWarningBadgedIcon() -> NSImage? {
        let basePalette = NSImage.SymbolConfiguration(pointSize: 16, weight: .regular)
            .applying(NSImage.SymbolConfiguration(paletteColors: [NSColor.labelColor]))
        guard let base = NSImage(systemSymbolName: "waveform.circle",
                                 accessibilityDescription: "Dimmy - Hotkey disabled")?
                .withSymbolConfiguration(basePalette) else { return nil }

        let badgePalette = NSImage.SymbolConfiguration(pointSize: 9, weight: .bold)
            .applying(NSImage.SymbolConfiguration(paletteColors: [NSColor.systemYellow]))
        let badge = NSImage(systemSymbolName: "exclamationmark.circle.fill",
                            accessibilityDescription: nil)?
            .withSymbolConfiguration(badgePalette)

        let size = NSSize(width: 18, height: 18)
        let composed = NSImage(size: size, flipped: false) { _ in
            let baseSize = base.size
            let baseOrigin = NSPoint(x: (size.width - baseSize.width) / 2,
                                     y: (size.height - baseSize.height) / 2)
            base.draw(in: NSRect(origin: baseOrigin, size: baseSize),
                      from: .zero, operation: .sourceOver, fraction: 1.0)

            if let badge {
                let badgeSize = NSSize(width: 10, height: 10)
                let origin = NSPoint(x: size.width - badgeSize.width,
                                     y: 0)
                badge.draw(in: NSRect(origin: origin, size: badgeSize),
                           from: .zero, operation: .sourceOver, fraction: 1.0)
            }
            return true
        }
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

    @objc private func togglePopover() {
        guard let popover, let button = statusItem?.button else { return }
        if popover.isShown {
            popover.performClose(nil)
        } else {
            popover.show(relativeTo: button.bounds, of: button, preferredEdge: .minY)
            popover.contentViewController?.view.window?.makeKey()
        }
    }
}

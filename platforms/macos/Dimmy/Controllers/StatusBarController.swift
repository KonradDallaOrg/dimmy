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

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
        observeRecordingState()
    }

    private func setupStatusItem() {
        statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)

        guard let button = statusItem?.button else { return }
        button.image = NSImage(systemSymbolName: "waveform.circle", accessibilityDescription: "Dimmy")
        button.image?.size = NSSize(width: 18, height: 18)
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

    private func observeRecordingState() {
        appState.$recordingState
            .receive(on: DispatchQueue.main)
            .sink { [weak self] state in
                self?.updateIcon(for: state)
            }
            .store(in: &cancellables)
    }

    private func updateIcon(for state: RecordingState) {
        guard let button = statusItem?.button else { return }
        switch state {
        case .idle:
            button.image = NSImage(systemSymbolName: "waveform.circle", accessibilityDescription: "Dimmy - Ready")
            button.image?.isTemplate = true
        case .recording:
            button.image = NSImage(systemSymbolName: "waveform.circle.fill", accessibilityDescription: "Dimmy - Recording")
            button.image?.isTemplate = false
            if let image = button.image {
                let config = NSImage.SymbolConfiguration(paletteColors: [.systemRed])
                button.image = image.withSymbolConfiguration(config)
            }
        case .transcribing:
            button.image = NSImage(systemSymbolName: "ellipsis.circle.fill", accessibilityDescription: "Dimmy - Transcribing")
            button.image?.isTemplate = false
            if let image = button.image {
                let config = NSImage.SymbolConfiguration(paletteColors: [.systemBlue])
                button.image = image.withSymbolConfiguration(config)
            }
        case .processing:
            button.image = NSImage(systemSymbolName: "sparkles", accessibilityDescription: "Dimmy - Processing")
            button.image?.isTemplate = false
            if let image = button.image {
                let config = NSImage.SymbolConfiguration(paletteColors: [.systemPurple])
                button.image = image.withSymbolConfiguration(config)
            }
        case .completing:
            button.image = NSImage(systemSymbolName: "checkmark.circle.fill", accessibilityDescription: "Dimmy - Done")
            button.image?.isTemplate = false
            if let image = button.image {
                let config = NSImage.SymbolConfiguration(paletteColors: [.systemGreen])
                button.image = image.withSymbolConfiguration(config)
            }
        }
        button.image?.size = NSSize(width: 18, height: 18)
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

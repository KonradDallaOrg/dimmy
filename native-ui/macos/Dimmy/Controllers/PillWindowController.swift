import AppKit
import SwiftUI
import Combine

// MARK: - Right-click aware hosting view for NSPanel

/// NSHostingView subclass that intercepts right-click to show a context menu.
/// SwiftUI's `.contextMenu` doesn't work reliably on borderless NSPanel,
/// so we handle it at the AppKit level.
final class PillHostingView<Content: View>: NSHostingView<Content> {
    var contextMenuProvider: (() -> NSMenu)?

    override func menu(for event: NSEvent) -> NSMenu? {
        contextMenuProvider?()
    }
}

@MainActor
final class PillWindowController {
    private var panel: NSPanel?
    private var appState: AppState
    private var cancellables = Set<AnyCancellable>()
    /// Flag to prevent didMove observer from writing back during programmatic repositioning
    private var isRepositioning = false

    // Extra padding around the pill so glow/shadow isn't clipped
    static let glowPadding: CGFloat = 20
    private let panelWidth: CGFloat = 280 + glowPadding * 2
    private let panelHeight: CGFloat = 56 + glowPadding * 2

    init(appState: AppState) {
        self.appState = appState
        setupPanel()
        observeState()
    }

    private func setupPanel() {
        let pillView = PillView(appState: appState)
            .padding(Self.glowPadding)

        let hostingView = PillHostingView(rootView: pillView)
        hostingView.contextMenuProvider = { [weak self] in
            self?.buildContextMenu() ?? NSMenu()
        }

        let panel = NSPanel(
            contentRect: NSRect(x: 0, y: 0, width: panelWidth, height: panelHeight),
            styleMask: [.borderless, .nonactivatingPanel],
            backing: .buffered,
            defer: false
        )

        panel.level = .floating
        panel.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary]
        panel.isMovableByWindowBackground = true
        panel.backgroundColor = .clear
        panel.isOpaque = false
        panel.hasShadow = false
        panel.hidesOnDeactivate = false
        panel.titleVisibility = .hidden
        panel.titlebarAppearsTransparent = true
        panel.acceptsMouseMovedEvents = true
        panel.contentView = hostingView

        positionPanel(panel, at: appState.pillPosition)

        self.panel = panel
    }

    func show() {
        panel?.orderFront(nil)
    }

    func hide() {
        panel?.orderOut(nil)
    }

    // MARK: - Positioning

    /// Position the panel at the given point, or default to top-right of screen.
    private func positionPanel(_ panel: NSPanel, at position: CGPoint?) {
        guard let screen = NSScreen.main else { return }
        let screenFrame = screen.visibleFrame
        let x: CGFloat
        let y: CGFloat

        if let pos = position {
            x = pos.x
            y = pos.y
        } else {
            // Default: top-right area
            x = screenFrame.maxX - panelWidth - 100
            y = screenFrame.maxY - panelHeight - 100
        }

        panel.setFrame(NSRect(x: x, y: y, width: panelWidth, height: panelHeight), display: true)
    }

    /// Move pill to default position (called by "Reset Position" in settings)
    private func resetToDefaultPosition() {
        guard let panel else { return }
        isRepositioning = true
        positionPanel(panel, at: nil)
        isRepositioning = false
    }

    // MARK: - Context menu (NSMenu, works on NSPanel)

    private func buildContextMenu() -> NSMenu {
        let menu = NSMenu()

        let settingsItem = NSMenuItem(title: "Settings...", action: #selector(settingsAction), keyEquivalent: ",")
        settingsItem.target = self
        menu.addItem(settingsItem)

        menu.addItem(.separator())

        let quitItem = NSMenuItem(title: "Quit Dimmy", action: #selector(quitAction), keyEquivalent: "q")
        quitItem.target = self
        menu.addItem(quitItem)

        return menu
    }

    @objc private func settingsAction() {
        AppDelegate.shared?.openSettings()
    }

    @objc private func quitAction() {
        NSApplication.shared.terminate(nil)
    }

    // MARK: - State observation

    private func observeState() {
        // Save position when user drags the pill
        NotificationCenter.default.publisher(for: NSWindow.didMoveNotification, object: panel)
            .compactMap { ($0.object as? NSWindow)?.frame.origin }
            .sink { [weak self] origin in
                guard let self, !self.isRepositioning else { return }
                self.appState.pillPosition = origin
            }
            .store(in: &cancellables)

        // Watch for position reset (nil means "go to default")
        appState.$pillPosition
            .dropFirst()
            .receive(on: DispatchQueue.main)
            .sink { [weak self] newPosition in
                guard let self, let panel = self.panel else { return }
                if newPosition == nil {
                    self.resetToDefaultPosition()
                }
            }
            .store(in: &cancellables)

        appState.$isOnboardingComplete
            .receive(on: DispatchQueue.main)
            .sink { [weak self] complete in
                if complete {
                    self?.show()
                }
            }
            .store(in: &cancellables)

        appState.$showPillIntro
            .receive(on: DispatchQueue.main)
            .sink { [weak self] show in
                if show {
                    self?.show()
                }
            }
            .store(in: &cancellables)
    }
}

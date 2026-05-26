import AppKit
import SwiftUI
import Combine

extension Notification.Name {
    /// Posted by `PillWindowController.stopMeetingFromPill` after the
    /// recap pipeline finishes. MeetingWindowController listens to
    /// refresh its sidebar + jump to the new row when the meeting
    /// window happens to be open.
    static let meetingRecapSaved = Notification.Name("dimmy.meetingRecapSaved")
}

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
    // Extra padding around the pill so glow/shadow isn't clipped — and so
    // the scroll-cycle popover (renders below the pill) has room to breathe
    // without being clipped by the panel bounds.
    static let glowPadding: CGFloat = 32
    private let panelWidth: CGFloat = 280 + glowPadding * 2
    private let panelHeight: CGFloat = 56 + glowPadding * 2

    init(appState: AppState) {
        self.appState = appState
        setupPanel()
        observeState()
        // One-shot initial sync in case a meeting is already active when
        // the pill is opened (e.g. user reopened the pill while a meeting
        // ran in the background). Subsequent updates flow through the
        // `meeting_state` event handler in DimmyCore.handleEvent — no
        // polling. Replaces the 0.5 s `meetingStatePollTimer` that lived
        // here before. See CLAUDE.md "No FFI-state polling rule".
        syncMeetingStateOnce()
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

    /// Show the pill, but only if the user hasn't hidden it via the
    /// menubar toggle. Callers (onboarding completion, intro nudge) just
    /// request "show" — the pillVisible preference vetoes when off.
    func show() {
        hkLog("[PillWindow] show() — pillVisible=\(appState.pillVisible) panel=\(panel != nil)")
        guard appState.pillVisible else { return }
        panel?.orderFront(nil)
    }

    func hide() {
        hkLog("[PillWindow] hide() — panel=\(panel != nil)")
        panel?.orderOut(nil)
    }

    // MARK: - Positioning

    /// Position the panel at the given point, or — when no explicit point is
    /// provided — at the corner / edge described by `appState.overlayPosition`.
    /// This honours the 3×2 grid in Settings → Pill overlay → Position.
    private func positionPanel(_ panel: NSPanel, at position: CGPoint?) {
        guard let screen = NSScreen.main else { return }
        let frame = screen.visibleFrame
        let margin: CGFloat = 20

        let x: CGFloat
        let y: CGFloat

        if let pos = position {
            x = pos.x
            y = pos.y
        } else {
            (x, y) = Self.defaultOrigin(
                for: appState.overlayPosition,
                frame: frame,
                panelSize: NSSize(width: panelWidth, height: panelHeight),
                margin: margin
            )
        }

        panel.setFrame(NSRect(x: x, y: y, width: panelWidth, height: panelHeight), display: true)
    }

    /// Map the user-facing position label to a screen-space origin.
    /// The panel is wider/taller than the visible pill by `2 * glowPadding`
    /// on each axis so shadows/glow don't get clipped. We compensate by
    /// subtracting `glowPadding` so the *visible* pill — not the invisible
    /// transparent gutter — ends up `margin` pt from the screen edge.
    /// Matches Windows' `WindowHelper.PositionByPreset` (margin = 20).
    private static func defaultOrigin(
        for label: String,
        frame: NSRect,
        panelSize: NSSize,
        margin: CGFloat
    ) -> (CGFloat, CGFloat) {
        let glow = glowPadding
        let leftX   = frame.minX + margin - glow
        let centerX = frame.midX - panelSize.width / 2
        let rightX  = frame.maxX - panelSize.width - margin + glow
        let bottomY = frame.minY + margin - glow
        let topY    = frame.maxY - panelSize.height - margin + glow

        switch label {
        case "Top Left":      return (leftX, topY)
        case "Top Center":    return (centerX, topY)
        case "Top Right":     return (rightX, topY)
        case "Bottom Left":   return (leftX, bottomY)
        case "Bottom Center": return (centerX, bottomY)
        case "Bottom Right":  return (rightX, bottomY)
        default:              return (rightX, bottomY)  // safe fallback
        }
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

        // Phase 6.2 — meeting entry point reachable from the pill right-
        // click in addition to tray + dock. Mirror of the Win flyout.
        let meetingItem = NSMenuItem(title: "Open Meeting…",
                                     action: #selector(openMeetingAction),
                                     keyEquivalent: "")
        meetingItem.target = self
        menu.addItem(meetingItem)

        let hideItem = NSMenuItem(title: "Hide pill", action: #selector(hidePillAction), keyEquivalent: "")
        hideItem.target = self
        menu.addItem(hideItem)

        menu.addItem(.separator())

        let quitItem = NSMenuItem(title: "Quit Dimmy", action: #selector(quitAction), keyEquivalent: "q")
        quitItem.target = self
        menu.addItem(quitItem)

        return menu
    }

    @objc private func settingsAction() {
        AppDelegate.shared?.openSettings()
    }

    @objc private func openMeetingAction() {
        AppDelegate.shared?.openMeetingWindow()
    }

    @objc private func hidePillAction() {
        appState.pillVisible = false
    }

    @objc private func quitAction() {
        NSApplication.shared.terminate(nil)
    }

    // MARK: - Meeting-state initial sync
    //
    // Steady-state updates flow through the `meeting_state` event in
    // DimmyCore.handleEvent (envelope from `dimmy_set_event_callback`).
    // This function only handles the one case the event channel CAN'T:
    // a meeting that was already active when the pill window opens
    // (no transition fires while we're attaching).

    private func syncMeetingStateOnce() {
        let active = DimmyCore.shared.meetingIsActive
        let paused = DimmyCore.shared.meetingIsPaused
        if active != appState.meetingActive {
            appState.meetingActive = active
        }
        if paused != appState.meetingIsPaused {
            appState.meetingIsPaused = paused
        }
    }

    // MARK: - Pill Stop → meeting recap

    /// Called by PillView's Stop button branch when a meeting is
    /// active. Spins up the recap pipeline on a background thread,
    /// shows transcribing-style feedback on the pill while the LLM
    /// runs, then drops the pill back to idle.
    nonisolated static func stopMeetingFromPill(appState: AppState) {
        Task { @MainActor in
            // Visual feedback during recap — recap can take 10-60s.
            appState.recordingState = .transcribing
            let notionAutoSend = appState.notionAutoSend
            await Task.detached(priority: .userInitiated) {
                let stopResult = DimmyCore.shared.meetingStop()
                let transcript = stopResult?.transcript
                    .trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
                guard let stopResult, !transcript.isEmpty else {
                    // Empty / failed stop → no recap will run. Still post
                    // meetingRecapSaved so a meeting window pinned in
                    // "Wrapping up…" (the $meetingActive→processing path)
                    // resolves to the empty done state instead of hanging
                    // forever waiting for a recap that never comes.
                    await MainActor.run {
                        appState.recordingState = .idle
                        appState.meetingActive = DimmyCore.shared.meetingIsActive
                        NotificationCenter.default.post(
                            name: .meetingRecapSaved,
                            object: nil,
                            userInfo: [
                                "dir": stopResult?.dir ?? "",
                                "success": false,
                            ]
                        )
                    }
                    return
                }
                let recap = MeetingPostProcessService.runRecap(
                    dir: stopResult.dir,
                    transcript: transcript,
                    notionAutoSend: notionAutoSend
                )
                await MainActor.run {
                    appState.recordingState = .completing
                    appState.meetingActive = DimmyCore.shared.meetingIsActive
                    NotificationCenter.default.post(
                        name: .meetingRecapSaved,
                        object: nil,
                        userInfo: [
                            "dir": stopResult.dir,
                            "success": (try? recap.get()) != nil,
                        ]
                    )
                    // Brief completion flash, then idle.
                    DispatchQueue.main.asyncAfter(deadline: .now() + 0.8) {
                        appState.recordingState = .idle
                    }
                }
            }.value
        }
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

        // Watch for the user picking a new corner in Settings → Pill →
        // Position. Drop the explicit drag-position so the panel snaps
        // to the chosen corner immediately.
        appState.$overlayPosition
            .dropFirst()
            .receive(on: DispatchQueue.main)
            .sink { [weak self] _ in
                guard let self else { return }
                self.appState.pillPosition = nil
                self.resetToDefaultPosition()
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

        // React to the menubar "Hide pill" toggle. Skip the initial value —
        // visibility on launch is owned by AppDelegate / onboarding flow.
        appState.$pillVisible
            .dropFirst()
            .receive(on: DispatchQueue.main)
            .sink { [weak self] visible in
                hkLog("[PillWindow] pillVisible changed → \(visible)")
                if visible { self?.show() } else { self?.hide() }
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

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
    nonisolated static let glowPadding: CGFloat = 32
    private let panelWidth: CGFloat = 280 + glowPadding * 2
    private let panelHeight: CGFloat = 56 + glowPadding * 2
    // Vertical pill on the left/right middle: tall, and wide enough for the
    // tip to sit beside the pill on the inner side.
    private let verticalPanelWidth: CGFloat = 220 + glowPadding * 2
    private let verticalPanelHeight: CGFloat = 280 + glowPadding * 2
    private var dragEndCheck: DispatchWorkItem?

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
        if appState.pillPosition != nil { snapToDock(animated: false) }
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
        appState.pillDock = ""
        positionPanel(panel, at: nil)
        isRepositioning = false
    }

    // MARK: - Snap to corners / edge middles (mirror of Win PillWindow.SnapToEdgeMiddle)

    /// AppKit has no "drag ended" for isMovableByWindowBackground: wait for
    /// the mouse button to come up after the last move.
    private func scheduleDragEndCheck() {
        dragEndCheck?.cancel()
        let item = DispatchWorkItem { [weak self] in
            guard let self else { return }
            if NSEvent.pressedMouseButtons & 1 != 0 {
                self.scheduleDragEndCheck()
            } else {
                self.zoneOverlay.hideAll()
                self.snapToDock(animated: true)
            }
        }
        dragEndCheck = item
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.12, execute: item)
    }

    /// Points between the visible pill's centre and an edge that still counts
    /// as "against" it. Same rule and numbers as Windows' PillDockMath (DIP).
    nonisolated static let edgeReach: CGFloat = 64
    /// Points either side of an edge's middle that dock to that middle.
    nonisolated static let middleBand: CGFloat = 120

    /// Where a dropped pill docks: a corner, the middle of an edge, or ""
    /// (it stays where it was dropped).
    nonisolated static func dock(pillCentre c: NSPoint, visible vf: NSRect) -> String {
        let dl = c.x - vf.minX, dr = vf.maxX - c.x, db = c.y - vf.minY, dt = vf.maxY - c.y
        let nearX = min(dl, dr) < edgeReach, nearY = min(dt, db) < edgeReach
        if nearX && nearY { return (dt < db ? "top" : "bottom") + (dl < dr ? "Left" : "Right") }
        if nearY && abs(c.x - vf.midX) < middleBand { return dt < db ? "top" : "bottom" }
        if nearX && abs(c.y - vf.midY) < middleBand { return dl < dr ? "left" : "right" }
        return ""
    }

    /// Points from a dock zone at which its outline appears during a drag.
    nonisolated static let approachReach: CGFloat = 3 * edgeReach

    /// Where the pill's CENTRE has to be for each dock: exactly where
    /// `dock(pillCentre:visible:)` answers that dock.
    nonisolated static func centreZones(visible vf: NSRect) -> [(dock: String, rect: NSRect)] {
        let r = edgeReach, b = middleBand
        return [
            ("topLeft", NSRect(x: vf.minX, y: vf.maxY - r, width: r, height: r)),
            ("topRight", NSRect(x: vf.maxX - r, y: vf.maxY - r, width: r, height: r)),
            ("bottomLeft", NSRect(x: vf.minX, y: vf.minY, width: r, height: r)),
            ("bottomRight", NSRect(x: vf.maxX - r, y: vf.minY, width: r, height: r)),
            ("top", NSRect(x: vf.midX - b, y: vf.maxY - r, width: 2 * b, height: r)),
            ("bottom", NSRect(x: vf.midX - b, y: vf.minY, width: 2 * b, height: r)),
            ("left", NSRect(x: vf.minX, y: vf.midY - b, width: r, height: 2 * b)),
            ("right", NSRect(x: vf.maxX - r, y: vf.midY - b, width: r, height: 2 * b)),
        ]
    }

    /// The zones drawn while dragging: each centre zone grown by the pill's
    /// radius (the area the docked pill covers), kept on screen.
    nonisolated static func dockZones(visible vf: NSRect) -> [(dock: String, rect: NSRect)] {
        let g = PillView.pillHeight / 2
        return centreZones(visible: vf).map { ($0.dock, $0.rect.insetBy(dx: -g, dy: -g).intersection(vf)) }
    }

    /// The docks whose zone the pill is close enough to for its outline to show.
    nonisolated static func nearDocks(pillCentre c: NSPoint, visible vf: NSRect) -> [String] {
        centreZones(visible: vf).filter { z in
            let dx = max(0, max(z.rect.minX - c.x, c.x - z.rect.maxX))
            let dy = max(0, max(z.rect.minY - c.y, c.y - z.rect.maxY))
            return (dx * dx + dy * dy).squareRoot() < approachReach
        }.map(\.dock)
    }

    /// The pill's centre inside a panel frame, from the alignment PillView uses.
    nonisolated static func pillCentre(in f: NSRect, alignment a: Alignment) -> NSPoint {
        let inset = glowPadding + PillView.pillHeight / 2
        let x = a.horizontal == .leading ? f.minX + inset : a.horizontal == .trailing ? f.maxX - inset : f.midX
        let y = a.vertical == .top ? f.maxY - inset : a.vertical == .bottom ? f.minY + inset : f.midY
        return NSPoint(x: x, y: y)
    }

    private let zoneOverlay = DockZoneOverlay()

    /// Faint outlines of the zones the pill is near, the one it would dock
    /// to if dropped now drawn stronger. Same rule as Windows.
    private func showDockZones() {
        guard let panel, let screen = panel.screen ?? NSScreen.main else { return }
        let vf = screen.visibleFrame
        let c = Self.pillCentre(in: panel.frame,
                                alignment: PillView.alignment(dock: appState.pillDock,
                                                              overlayPosition: appState.overlayPosition))
        zoneOverlay.show(zones: Self.dockZones(visible: vf),
                       near: Self.nearDocks(pillCentre: c, visible: vf),
                       active: Self.dock(pillCentre: c, visible: vf),
                       below: panel)
    }

    private func snapToDock(animated: Bool) {
        guard let panel, let screen = panel.screen ?? NSScreen.main else { return }
        let vf = screen.visibleFrame
        // The pill the user sees, not the panel's centre (nor the cursor: over
        // a remote session the cursor read at mouse-up was unreliable). The
        // panel is several times larger and, docked, the pill sits against
        // one of its sides.
        let c = Self.pillCentre(in: panel.frame,
                                alignment: PillView.alignment(dock: appState.pillDock,
                                                              overlayPosition: appState.overlayPosition))
        let dock = Self.dock(pillCentre: c, visible: vf)

        let vertical = dock == "left" || dock == "right"
        let w = vertical ? verticalPanelWidth : panelWidth
        let h = vertical ? verticalPanelHeight : panelHeight
        let glow = Self.glowPadding
        let m: CGFloat = 8
        let left = vf.minX + m - glow, right = vf.maxX - w - m + glow
        let bottom = vf.minY + m - glow, top = vf.maxY - h - m + glow
        let midX = vf.midX - w / 2, midY = vf.midY - h / 2

        let origin: NSPoint
        switch dock {
        case "left": origin = NSPoint(x: left, y: midY)
        case "right": origin = NSPoint(x: right, y: midY)
        case "top": origin = NSPoint(x: midX, y: top)
        case "bottom": origin = NSPoint(x: midX, y: bottom)
        case "topLeft": origin = NSPoint(x: left, y: top)
        case "topRight": origin = NSPoint(x: right, y: top)
        case "bottomLeft": origin = NSPoint(x: left, y: bottom)
        case "bottomRight": origin = NSPoint(x: right, y: bottom)
        default:
            let f = panel.frame
            origin = (f.width == w && f.height == h) ? f.origin : NSPoint(x: f.midX - w / 2, y: f.midY - h / 2)
        }
        let target = NSRect(origin: origin, size: NSSize(width: w, height: h))

        hkLog("[PillWindow] snap pill=\(Int(c.x)),\(Int(c.y)) visible=\(vf) dock=\(dock.isEmpty ? "none" : dock)")
        appState.pillDock = dock
        guard target != panel.frame else { return }
        isRepositioning = true
        let finish = { [weak self] in
            guard let self else { return }
            self.isRepositioning = false
            self.appState.pillPosition = panel.frame.origin
        }
        if animated {
            NSAnimationContext.runAnimationGroup({ ctx in
                ctx.duration = 0.22
                ctx.timingFunction = CAMediaTimingFunction(name: .easeOut)
                panel.animator().setFrame(target, display: true)
            }, completionHandler: { Task { @MainActor in finish() } })
        } else {
            panel.setFrame(target, display: true)
            finish()
        }
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

        menu.addItem(.separator())

        // The three primary actions, mirrored across pill / status-bar / Dock.
        // The shortcut combo is shown inline as text when set; no keyEquivalent
        // (the global CGEventTap owns the shortcuts — a menu key-equivalent
        // would double-fire while the app is frontmost).
        let dictTitle = appState.isRecording ? "Stop Dictation" : "Start Dictation"
        let dictItem = NSMenuItem(title: dictTitle,
                                  action: #selector(toggleDictationAction),
                                  keyEquivalent: "")
        dictItem.target = self
        menu.addItem(dictItem)

        // Command (next dictation) — arms a one-shot command (does not record
        // now). ✓ when armed. Replaces the old sticky command toggle.
        let commandTitle = appState.commandHotkey
            .map { "Command (next dictation)   \($0.displayString)" }
            ?? "Command (next dictation)"
        let commandItem = NSMenuItem(title: commandTitle,
                                     action: #selector(armCommandAction),
                                     keyEquivalent: "")
        commandItem.target = self
        commandItem.state = appState.oneShotCommandPending ? .on : .off
        menu.addItem(commandItem)

        // Start/Stop Meeting — consent-gated background toggle.
        let mtgTitle = appState.meetingActive ? "Stop Meeting" : "Start Meeting"
        let mtgFull = appState.meetingHotkey
            .map { "\(mtgTitle)   \($0.displayString)" } ?? mtgTitle
        let mtgItem = NSMenuItem(title: mtgFull,
                                 action: #selector(toggleMeetingAction),
                                 keyEquivalent: "")
        mtgItem.target = self
        menu.addItem(mtgItem)

        menu.addItem(.separator())

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
        hkLog("[PillWindow] context menu: Hide pill")
        appState.pillVisible = false
    }

    @objc private func toggleDictationAction() {
        if !DimmyCore.shared.isInitialized {
            DispatchQueue.global(qos: .userInitiated).async {
                _ = DimmyCore.shared.initialize()
                DispatchQueue.main.async { HotkeyManager.shared.toggleRecordingFromUI() }
            }
            return
        }
        HotkeyManager.shared.toggleRecordingFromUI()
    }

    @objc private func armCommandAction() {
        appState.oneShotCommandPending.toggle()
        hkLog("[CmdMode] pill menu one-shot armed → \(appState.oneShotCommandPending)")
    }

    @objc private func toggleMeetingAction() {
        if !DimmyCore.shared.isInitialized {
            DispatchQueue.global(qos: .userInitiated).async {
                _ = DimmyCore.shared.initialize()
                DispatchQueue.main.async { MeetingShortcut.toggle(appState: self.appState, source: "menu") }
            }
            return
        }
        MeetingShortcut.toggle(appState: appState, source: "menu")
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

    @MainActor private static var stopInFlight = false

    /// The one way a meeting stops: the pill, the meeting window's Stop,
    /// the meeting shortcut and the call-detect popup all land here (Win
    /// mirror: MeetingStopService). Spins up the recap pipeline on a
    /// background thread. It is background work, like a loaded file: the
    /// pill says "Transcribing..." through `transcribesRunning` until the
    /// transcript is final, then "Recap..." through `recapsRunning`, and the
    /// dictation state is never touched, so a dictation can start while the
    /// meeting saves. An open meeting window follows through
    /// `meetingRecapSaved` (an empty `dir` = the stop failed).
    nonisolated static func stopMeetingFromPill(appState: AppState) {
        Task { @MainActor in
            // A second press while a stop runs would reach the core as a stop
            // of nothing.
            guard !stopInFlight else {
                dimmyHostLog("[Meeting] stop already running — this press is dropped")
                return
            }
            stopInFlight = true
            defer { stopInFlight = false }
            appState.transcribesRunning += 1
            let notionAutoSend = appState.notionAutoSend
            // Capture the recap flag pinned at meeting START (set by
            // MeetingViewModel.start / call-detect-started) so a stale
            // checkbox on a reopened window can't override the user's
            // choice. Pill + call-detect popup stop paths used to run
            // the recap unconditionally — fixed for cross-path parity
            // with the meeting-window stop button (Win parity).
            let generateRecap = appState.meetingGenerateRecap
            // Tear down the system-audio tap BEFORE the core stop, same
            // ordering as the window path (MeetingViewModel.stopAndProcess).
            // Without this, a pill/hotkey/nudge stop leaked a live tap
            // (purple indicator stays on) AND left isRunning=true, which
            // silently no-op'd the capture start of the NEXT meeting —
            // burned 2026-07-03. Idempotent when capture never started.
            SystemAudioCaptureService.shared.stop()
            await Task.detached(priority: .userInitiated) {
                let stopResult = DimmyCore.shared.meetingStop()
                // WAV finalize errors (disk full, IO) ride MeetingResult.
                // The meeting-window stop path appends them to the done
                // meta; this path (pill + hotkey + call-nudge all land
                // here) swallowed them. Win parity: StopMeetingFromPill
                // toast via ShowMeetingAudioIncomplete.
                if let stopError = stopResult?.error {
                    dimmyHostLog("[Pill stop] meeting stop reported error: \(stopError)")
                    await MainActor.run {
                        DictToastWindow.show(
                            kind: .error,
                            title: "Meeting audio incomplete",
                            body: stopError)
                    }
                }
                // Speaker labels (when enabled) replace the live per-track
                // transcript before the recap reads it.
                let transcript = stopResult.map {
                    DiarizationService.relabelIfEnabled(dir: $0.dir, liveTranscript: $0.transcript,
                                                        alreadyLabeled: $0.speakersLabeled)
                }?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
                await MainActor.run {
                    appState.transcribesRunning = max(0, appState.transcribesRunning - 1)
                    if stopResult == nil { appState.showError("Meeting stop failed") }
                }
                guard let stopResult, !transcript.isEmpty else {
                    dimmyHostLog("[Recap] skip (pill stop): reason=\(stopResult == nil ? "meetingStop returned nil" : "empty transcript") — no recap will run")
                    // Empty / failed stop → no recap will run. Still post
                    // meetingRecapSaved so a meeting window pinned in
                    // "Wrapping up…" (the $meetingActive→processing path)
                    // resolves to the empty done state instead of hanging
                    // forever waiting for a recap that never comes.
                    await MainActor.run {
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
                // Recap-off path: meeting stopped cleanly with a real
                // transcript, but the user toggled off "Generate recap".
                // Skip the LLM call AND post meetingRecapSaved with the
                // dir immediately, so the meeting window's $meetingActive
                // sink (which already flipped to .processing) resolves to
                // .done via loadDoneFromDisk instead of sitting until the
                // 90 s wrap-up watchdog fires. `recapSkipped: true` is
                // informational for any future consumer; the existing
                // observer only reads `dir`.
                if !generateRecap {
                    dimmyHostLog("[Recap] skip (pill stop): reason=meetingGenerateRecap=false (recap toggled off for this meeting) — transcript kept, no recap")
                    await MainActor.run {
                        appState.meetingActive = DimmyCore.shared.meetingIsActive
                        NotificationCenter.default.post(
                            name: .meetingRecapSaved,
                            object: nil,
                            userInfo: [
                                "dir": stopResult.dir,
                                "success": true,
                                "recapSkipped": true,
                            ]
                        )
                    }
                    return
                }
                // The transcript is final; the pill now shows "Recap..."
                // (recapsRunning, raised inside runRecap).
                let recap = MeetingPostProcessService.runRecap(
                    dir: stopResult.dir,
                    transcript: transcript,
                    notionAutoSend: notionAutoSend
                )
                await MainActor.run {
                    appState.meetingActive = DimmyCore.shared.meetingIsActive
                    NotificationCenter.default.post(
                        name: .meetingRecapSaved,
                        object: nil,
                        userInfo: [
                            "dir": stopResult.dir,
                            "success": (try? recap.get()) != nil,
                        ]
                    )
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
                // Only a user drag (button held) ends in a snap.
                if NSEvent.pressedMouseButtons & 1 != 0 {
                    self.showDockZones()
                    self.scheduleDragEndCheck()
                }
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

/// The dock zones drawn while the pill is dragged (mirror of Windows'
/// DockZoneOverlay): one click-through panel per zone, translucent sky blue
/// with rounded corners, kept just below the pill. Only touched when a
/// zone's state or place changes.
@MainActor
final class DockZoneOverlay {
    private enum ZoneState { case hidden, near, active }
    private var panels: [String: NSPanel] = [:]
    private var states: [String: ZoneState] = [:]
    private static let color = NSColor(srgbRed: 0x38 / 255.0, green: 0xBD / 255.0, blue: 0xF8 / 255.0, alpha: 1)

    func show(zones: [(dock: String, rect: NSRect)], near: [String], active: String, below pill: NSWindow) {
        for z in zones {
            let want: ZoneState = z.dock == active ? .active : (near.contains(z.dock) ? .near : .hidden)
            let panel = panels[z.dock] ?? makePanel(for: z.dock)
            if panel.frame != z.rect { panel.setFrame(z.rect, display: false) }
            guard states[z.dock] != want else { continue }
            states[z.dock] = want
            switch want {
            case .hidden:
                panel.orderOut(nil)
            case .near, .active:
                // On the rounded layer, not the window: a window's background
                // ignores its content view's corner radius.
                panel.contentView?.layer?.backgroundColor = Self.color.withAlphaComponent(want == .active ? 0.5 : 0.24).cgColor
                panel.order(.below, relativeTo: pill.windowNumber)
            }
        }
    }

    func hideAll() {
        for (dock, panel) in panels where states[dock] != .hidden {
            panel.orderOut(nil)
            states[dock] = .hidden
        }
    }

    private func makePanel(for dock: String) -> NSPanel {
        let p = NSPanel(contentRect: .zero, styleMask: [.borderless, .nonactivatingPanel],
                        backing: .buffered, defer: true)
        p.isOpaque = false
        p.backgroundColor = .clear
        p.hasShadow = false
        p.ignoresMouseEvents = true
        p.level = .floating
        p.collectionBehavior = [.canJoinAllSpaces, .stationary, .ignoresCycle]
        p.contentView?.wantsLayer = true
        p.contentView?.layer?.cornerRadius = 12
        p.contentView?.layer?.masksToBounds = true
        panels[dock] = p
        states[dock] = .hidden
        return p
    }
}

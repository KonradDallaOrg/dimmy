import AppKit
import SwiftUI

// MARK: - MeetingWindowController
//
// Owns the single meeting NSWindow. The window stays alive for the
// lifetime of the app (`isReleasedWhenClosed = false`), so closing
// just hides it — recording state lives in the Rust core (the
// `MEETING` static), which `MeetingViewModel.onWindowShown` re-attaches
// to whenever the window comes back. This decouples lifecycle from
// FFI in the same shape Win does after the system-audio-capture port.
//
// Kept deliberately thin: the heavy lifting (state machine, polling,
// FFI orchestration, sidebar) lives in MeetingViewModel + the modular
// SwiftUI views under `Views/Meeting/`.

@MainActor
final class MeetingWindowController {
    static let shared = MeetingWindowController()

    private var window: NSWindow?
    let viewModel = MeetingViewModel()
    private var recapSavedObserver: NSObjectProtocol?

    private init() {
        // Mirror of Win `App.NotifyMeetingRecapSaved` → MeetingWindow
        // refresh hook. When the pill stops a meeting and the recap
        // pipeline finishes, the meeting window (if open) reloads its
        // sidebar and selects the freshly-finished row.
        recapSavedObserver = NotificationCenter.default.addObserver(
            forName: .meetingRecapSaved,
            object: nil,
            queue: .main
        ) { [weak self] note in
            Task { @MainActor in
                guard let self else { return }
                self.viewModel.loadHistory()
                if let dir = note.userInfo?["dir"] as? String {
                    self.viewModel.loadDoneFromDisk(dir: dir)
                }
            }
        }
    }

    deinit {
        if let recapSavedObserver {
            NotificationCenter.default.removeObserver(recapSavedObserver)
        }
    }

    /// Show (or activate) the meeting window. If a meeting is in
    /// flight when called — say the user closed the window mid-
    /// recording, or the meeting was kicked off from the pill /
    /// menu bar — `MeetingView.onAppear` calls
    /// `viewModel.onWindowShown()` which probes
    /// `dimmy_meeting_is_active()` and re-attaches.
    func show() {
        if let window {
            window.makeKeyAndOrderFront(nil)
            NSApp.activate(ignoringOtherApps: true)
            // Re-fire so reattach + history reload happens even when
            // the window was hidden (onAppear doesn't fire on
            // `makeKeyAndOrderFront` of an already-living window).
            viewModel.onWindowShown()
            return
        }

        let view = MeetingView(vm: viewModel)
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 920, height: 620),
            styleMask: [.titled, .closable, .miniaturizable, .resizable, .fullSizeContentView],
            backing: .buffered,
            defer: false
        )
        window.title = "Dimmy — Meeting"
        window.titlebarAppearsTransparent = true
        window.titleVisibility = .hidden
        window.isReleasedWhenClosed = false
        window.center()
        window.contentView = NSHostingView(rootView: view)
        window.minSize = NSSize(width: 880, height: 560)
        window.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
        self.window = window
    }
}

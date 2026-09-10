import AppKit
import SwiftUI

// MARK: - WizardWindowController
//
// Owns the two setup-wizard windows (Command mode, Meeting mode).
//
// One controller rather than two near-identical singletons: the windows
// differ only in title, size and root view, and both want the same
// show-or-raise, close-on-finish and survive-a-close behaviour. Mirror of
// `CommandWizardWindow` / `MeetingWizardWindow` on Windows, which are two
// classes only because WinUI has no equivalent of holding a window in a
// dictionary.
//
// Both windows outlive being closed (`isReleasedWhenClosed = false`), so the
// wizard can be reopened from the dashboard as often as the user likes and
// picks up wherever the settings now are — the steps read live config, they
// hold no state of their own worth preserving.

@MainActor
final class WizardWindowController {
    static let shared = WizardWindowController()

    enum Kind {
        case command
        case meeting
    }

    private var commandWindow: NSWindow?
    private var meetingWindow: NSWindow?

    private init() {}

    func show(_ kind: Kind, appState: AppState) {
        if let existing = window(for: kind) {
            existing.makeKeyAndOrderFront(nil)
            NSApp.activate(ignoringOtherApps: true)
            return
        }

        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0,
                                width: kind == .meeting ? 620 : 560,
                                height: kind == .meeting ? 560 : 500),
            styleMask: [.titled, .closable, .fullSizeContentView],
            backing: .buffered,
            defer: false
        )
        window.title = kind == .meeting ? "Set up Meeting mode" : "Set up Command mode"
        window.titlebarAppearsTransparent = true
        window.titleVisibility = .hidden
        window.isReleasedWhenClosed = false
        // The cards hover, and the wizard is opened from another window, so it
        // needs both of these: mouse-moved events for `.onHover`, and
        // first-mouse so the first click on Continue is not swallowed just
        // activating the window.
        window.acceptsMouseMovedEvents = true
        window.center()

        // The root view closes its own window when it finishes. Capturing the
        // window here rather than routing through the controller keeps the
        // wizard views free of any knowledge of who owns them.
        let finish: () -> Void = { [weak window] in window?.performClose(nil) }
        switch kind {
        case .command:
            window.contentView = FirstMouseHostingView(
                rootView: CommandWizardView(appState: appState, onFinish: finish))
            commandWindow = window
        case .meeting:
            window.contentView = FirstMouseHostingView(
                rootView: MeetingWizardView(appState: appState, onFinish: finish))
            meetingWindow = window
        }

        window.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
    }

    private func window(for kind: Kind) -> NSWindow? {
        switch kind {
        case .command: return commandWindow
        case .meeting: return meetingWindow
        }
    }
}

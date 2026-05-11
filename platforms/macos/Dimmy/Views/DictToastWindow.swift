import AppKit
import SwiftUI

/// Self-dismissing in-app toast for dictionary feedback. Bottom-right
/// of the active screen, ~3 s lifetime. Bespoke window instead of
/// `UNUserNotification` because we want immediate visual feedback
/// inline with the user's flow — UN-pushed notifications are
/// throttled, batched, and may not surface at all if the user has
/// Focus mode on. Same rationale as the Win-side `DictToastWindow`.
@MainActor
final class DictToastWindow {

    private static var current: NSWindow?
    private static var dismissTimer: Timer?

    /// Single entry point — both `Added` and `AlreadyPresent` variants
    /// just differ in title/body. Closing any previous toast before
    /// showing a new one prevents toast stacking when the user
    /// hammers the hotkey.
    static func show(title: String, body: String) {
        dismissCurrent()

        let content = DictToastView(title: title, message: body)
        let host = NSHostingController(rootView: content)
        let windowSize = NSSize(width: 380, height: 80)

        let window = NSWindow(
            contentRect: NSRect(origin: .zero, size: windowSize),
            styleMask: [.borderless],
            backing: .buffered,
            defer: false
        )
        window.contentViewController = host
        window.isOpaque = false
        window.backgroundColor = .clear
        window.hasShadow = true
        window.level = .floating
        // Survive Mission Control / fullscreen apps so the user still
        // sees the confirmation even when they're focused elsewhere.
        window.collectionBehavior = [.canJoinAllSpaces, .stationary, .ignoresCycle]
        window.isMovable = false

        // Anchor: bottom-right of the screen containing the mouse
        // pointer (= where the user just pressed the hotkey). Margin
        // of 16 px from each visible edge matches the Win toast.
        if let screen = NSScreen.screens.first(where: { $0.frame.contains(NSEvent.mouseLocation) })
            ?? NSScreen.main {
            let visible = screen.visibleFrame
            let origin = NSPoint(
                x: visible.maxX - windowSize.width - 16,
                y: visible.minY + 16
            )
            window.setFrameOrigin(origin)
        }

        window.makeKeyAndOrderFront(nil)
        // Don't actually steal focus from whatever app the user is in —
        // they triggered this from another app and we don't want their
        // typing to land on our toast.
        NSApp.activate(ignoringOtherApps: false)

        current = window
        dismissTimer = Timer.scheduledTimer(withTimeInterval: 3.0, repeats: false) { _ in
            Task { @MainActor in dismissCurrent() }
        }
    }

    static func showAdded(word: String) {
        show(
            title: "Added to dictionary",
            body: "“\(word)” will boost recognition on future transcriptions."
        )
    }

    static func showAlreadyPresent(word: String) {
        show(
            title: "Already in dictionary",
            body: "“\(word)” is already on the list."
        )
    }

    static func dismissCurrent() {
        dismissTimer?.invalidate()
        dismissTimer = nil
        current?.orderOut(nil)
        current?.close()
        current = nil
    }
}

/// SwiftUI body of the toast. Adapts to the system colour scheme via
/// the standard `Color`/`material` semantic tokens — no bespoke
/// palette switch needed on Mac because NSWindow's effectiveAppearance
/// already propagates light/dark and SwiftUI's adaptive colours
/// resolve correctly. The Win side has to hand-paint two palettes
/// because the bespoke transparent backdrop bypasses theme resources.
private struct DictToastView: View {
    // Renamed from `body` to `message` to avoid colliding with the
    // SwiftUI `var body: some View` requirement — having both as
    // stored property + computed property is a Swift redeclaration
    // error.
    let title: String
    let message: String

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            Image(systemName: "text.badge.plus")
                .font(.system(size: 20, weight: .semibold))
                .foregroundStyle(.tint)
                .frame(width: 24, height: 24)
                .padding(.top, 2)
            VStack(alignment: .leading, spacing: 2) {
                Text(title)
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundStyle(.primary)
                Text(message)
                    .font(.system(size: 12))
                    .foregroundStyle(.secondary)
                    .lineLimit(2)
            }
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 12)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .leading)
        .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 10))
        .overlay(
            RoundedRectangle(cornerRadius: 10)
                .stroke(Color.primary.opacity(0.1), lineWidth: 1)
        )
        .onTapGesture {
            // Click-to-dismiss for impatient users.
            Task { @MainActor in DictToastWindow.dismissCurrent() }
        }
    }
}

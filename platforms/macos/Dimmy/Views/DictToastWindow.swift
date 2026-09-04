import AppKit
import SwiftUI

/// Self-dismissing in-app toast for dictionary feedback. Top-centre of
/// the active screen; it stays up long enough to read (see
/// `visibleSeconds`). Bespoke window instead of
/// `UNUserNotification` because we want immediate visual feedback
/// inline with the user's flow — UN-pushed notifications are
/// throttled, batched, and may not surface at all if the user has
/// Focus mode on. Same rationale as the Win-side `DictToastWindow`.
///
/// Layout note: the toast lives near the top of the screen instead of
/// the bottom because users normally focus their attention on the top
/// half of their display (menu bar, browser tabs, document headers).
/// A bottom-right toast on a 27" screen sits in the dead zone of the
/// user's vision and gets missed — empirically the #1 complaint from
/// the first internal test pass.
@MainActor
final class DictToastWindow {

    /// What happened. Drives the icon + accent colour. Title/body text
    /// is still set by the caller so we can localise + reuse the same
    /// styled chrome for future variants (e.g. "Removed", "Error").
    enum Kind {
        case added
        case alreadyPresent
        case workflowHint   // user pressed combo without re-copying
        case error          // command-mode failed (no LLM key, etc.)

        var symbolName: String {
            switch self {
            case .added:          return "checkmark.circle.fill"
            case .alreadyPresent: return "info.circle.fill"
            case .workflowHint:   return "hand.point.up.left.fill"
            case .error:          return "exclamationmark.triangle.fill"
            }
        }

        /// Semantic colour. Resolves correctly in both light and dark
        /// modes because SwiftUI's named `Color.green` / `.blue` / `.orange`
        /// adapt to system appearance.
        var accent: Color {
            switch self {
            case .added:          return .green
            case .alreadyPresent: return .blue
            case .workflowHint:   return .orange
            case .error:          return .red
            }
        }
    }

    private static var current: NSWindow?
    private static var dismissTimer: Timer?

    /// Single entry point. Closing any previous toast before showing a
    /// new one prevents toast stacking when the user hammers the
    /// hotkey.
    static func show(kind: Kind, title: String, body: String) {
        dismissCurrent()

        let content = DictToastView(kind: kind, title: title, message: body)
        let host = NSHostingController(rootView: content)
        let windowSize = NSSize(width: 480, height: 112)

        let window = NSWindow(
            contentRect: NSRect(origin: .zero, size: windowSize),
            styleMask: [.borderless],
            backing: .buffered,
            defer: false
        )
        // SIGSEGV in -[_NSWindowTransformAnimation dealloc] when close()
        // races a pending QuartzCore transaction. The window is the only
        // strong reference holder (we keep it in `current`); letting ARC
        // free it when we nil that out is safe, the AppKit-managed close
        // dealloc is not. Same pattern Apple recommends for transient
        // utility panels.
        window.isReleasedWhenClosed = false
        window.contentViewController = host
        window.isOpaque = false
        window.backgroundColor = .clear
        window.hasShadow = true
        // .statusBar = above .floating, ensures the toast sits on top of
        // other floating panels (e.g. Spotlight, system pickers).
        window.level = .statusBar
        // Survive Mission Control / fullscreen apps so the user still
        // sees the confirmation even when they're focused elsewhere.
        window.collectionBehavior = [.canJoinAllSpaces, .stationary, .ignoresCycle, .fullScreenAuxiliary]
        window.isMovable = false
        // Don't grab key window status — the user is typing in another
        // app, their keystrokes must stay there.
        window.ignoresMouseEvents = false  // still tappable to dismiss

        // Bottom-right anchor on the screen containing the mouse — same
        // corner as macOS Notification Center used to sit, where users
        // already instinctively look for non-disruptive confirmations.
        // 24 px margin on both edges keeps it clear of the Dock.
        if let screen = NSScreen.screens.first(where: { $0.frame.contains(NSEvent.mouseLocation) })
            ?? NSScreen.main {
            let visible = screen.visibleFrame
            let origin = NSPoint(
                x: visible.maxX - windowSize.width - 24,
                y: visible.minY + 24
            )
            window.setFrameOrigin(origin)
        }

        // orderFrontRegardless = visible WITHOUT becoming key window.
        // makeKeyAndOrderFront would steal first-responder, which is
        // exactly the bug we want to avoid — the user is typing
        // somewhere else and shouldn't lose focus to a toast.
        window.orderFrontRegardless()

        current = window
        dismissTimer = Timer.scheduledTimer(
            withTimeInterval: visibleSeconds(title: title, body: body), repeats: false
        ) { _ in
            Task { @MainActor in dismissCurrent() }
        }
    }

    /// How long to leave the toast up, from how long it takes to read.
    ///
    /// This was a flat 4 s, which suits "Added to dictionary" and nothing
    /// else. The toasts added in 2026-09 carry the user's next step — "try
    /// Parakeet or a smaller model", "the transcript is saved, you can run
    /// the recap again" — and those disappeared before they could be read
    /// (reported on Windows 2026-09-04; the Mac had the same flat timer).
    ///
    /// Win parity: `Services/ToastDuration.cs`, same constants.
    nonisolated static func visibleSeconds(title: String, body: String) -> TimeInterval {
        let minSecs = 3.0
        let maxSecs = 12.0
        let wordsPerMinute = 180.0   // glanced at, not read attentively
        let noticeSecs = 1.2         // finding the toast before reading it

        let words = (title + " " + body)
            .split(whereSeparator: { $0.isWhitespace })
            .count
        let total = noticeSecs + Double(words) / wordsPerMinute * 60.0
        return Swift.min(Swift.max(total, minSecs), maxSecs)
    }

    static func showAdded(word: String) {
        show(
            kind: .added,
            title: "Added “\(word)” to dictionary",
            body: "Will boost recognition on future transcriptions."
        )
    }

    static func showAlreadyPresent(word: String) {
        show(
            kind: .alreadyPresent,
            title: "“\(word)” already in dictionary",
            body: "Already on the list — Cmd+C a new selection, then press your hotkey again."
        )
    }

    /// Fires when the user presses the dict hotkey without re-copying.
    /// Detected via NSPasteboard.changeCount staying identical across
    /// two consecutive presses — see DictHotkeyManager.handleCarbonHotKey.
    /// Bigger hint than "alreadyPresent" because the root cause is
    /// different: user just forgot the Cmd+C step.
    static func showWorkflowHint(hotkey: String) {
        show(
            kind: .workflowHint,
            title: "Press Cmd+C first",
            body: "Without Accessibility, Dimmy reads your clipboard. Steps: select word → Cmd+C → \(hotkey)."
        )
    }

    static func dismissCurrent() {
        dismissTimer?.invalidate()
        dismissTimer = nil
        // orderOut hides the window from the screen. We deliberately
        // do NOT call close() — with releasedWhenClosed = false it
        // would be a no-op tear-down, but it still ends up calling
        // into the AppKit transform-animation cleanup path that
        // SIGSEGVs against an in-flight QuartzCore transaction.
        // Nil-ing `current` is the only release we need.
        current?.orderOut(nil)
        current = nil
    }
}

/// SwiftUI body of the toast. Adapts to the system colour scheme via
/// the standard `Color` / `material` semantic tokens — no bespoke
/// palette switch needed on Mac because NSWindow's effectiveAppearance
/// already propagates light/dark and SwiftUI's adaptive colours
/// resolve correctly. The Win side has to hand-paint two palettes
/// because the bespoke transparent backdrop bypasses theme resources.
private struct DictToastView: View {
    let kind: DictToastWindow.Kind
    let title: String
    let message: String

    var body: some View {
        HStack(alignment: .center, spacing: 14) {
            Image(systemName: kind.symbolName)
                .font(.system(size: 28, weight: .semibold))
                .foregroundStyle(kind.accent)
                .frame(width: 36, height: 36)
            VStack(alignment: .leading, spacing: 4) {
                Text(title)
                    .font(.system(size: 15, weight: .semibold))
                    .foregroundStyle(.primary)
                Text(message)
                    .font(.system(size: 13))
                    .foregroundStyle(.secondary)
                    .lineLimit(2)
            }
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 16)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .leading)
        .background(.thickMaterial, in: RoundedRectangle(cornerRadius: 14))
        .overlay(
            RoundedRectangle(cornerRadius: 14)
                .stroke(kind.accent.opacity(0.35), lineWidth: 1.5)
        )
        .shadow(color: .black.opacity(0.25), radius: 12, x: 0, y: 4)
        .onTapGesture {
            // Click-to-dismiss for impatient users.
            Task { @MainActor in DictToastWindow.dismissCurrent() }
        }
    }
}

import AppKit
import SwiftUI

/// Short feedback after a hotkey: "added to dictionary", "press Cmd+C
/// first", "command failed". Drawn as a macOS notification banner (top
/// right under the menu bar, real behind-window translucency, the app icon
/// with a small status badge) but shown by Dimmy itself: a system
/// notification needs a permission prompt, lands in Notification Center and
/// is hidden by Focus, which is wrong for feedback on a key the user just
/// pressed.
///
/// The first version was a 480x112 slab in the bottom-right corner with a
/// big coloured symbol and a coloured border; its SwiftUI material sat in a
/// transparent window, so it blurred nothing and read as flat grey.
@MainActor
final class DictToastWindow {

    enum Kind {
        case added
        case alreadyPresent
        case workflowHint   // user pressed combo without re-copying
        case error          // command-mode failed (no LLM key, etc.)

        /// Glyph of the badge on the app icon.
        var badgeSymbol: String {
            switch self {
            case .added:          return "checkmark"
            case .alreadyPresent: return "info"
            case .workflowHint:   return "hand.point.up.left.fill"
            case .error:          return "exclamationmark"
            }
        }

        var accent: Color {
            switch self {
            case .added:          return .green
            case .alreadyPresent: return .blue
            case .workflowHint:   return .orange
            case .error:          return .red
            }
        }
    }

    static let width: CGFloat = 360
    private static let margin: CGFloat = 12
    private static let cornerRadius: CGFloat = 16

    private static var current: NSPanel?
    private static var dismissTimer: Timer?

    /// Single entry point. Closing any previous toast before showing a
    /// new one prevents toast stacking when the user hammers the
    /// hotkey.
    static func show(kind: Kind, title: String, body: String) {
        dismissCurrent(animated: false)

        let host = NSHostingView(rootView: DictToastView(kind: kind, title: title, message: body))
        let size = NSSize(width: width, height: ceil(host.fittingSize.height))

        let panel = NSPanel(
            contentRect: NSRect(origin: .zero, size: size),
            styleMask: [.borderless, .nonactivatingPanel],
            backing: .buffered,
            defer: false
        )
        panel.isReleasedWhenClosed = false
        panel.isOpaque = false
        panel.backgroundColor = .clear
        panel.hasShadow = true
        // .statusBar = above .floating, so the toast sits on top of other
        // floating panels (Spotlight, system pickers).
        panel.level = .statusBar
        // Survive Mission Control / fullscreen apps so the user still sees
        // the confirmation while focused elsewhere.
        panel.collectionBehavior = [.canJoinAllSpaces, .stationary, .ignoresCycle, .fullScreenAuxiliary]
        panel.isMovable = false
        panel.hidesOnDeactivate = false

        // Translucent like a system banner: the effect view blurs what is
        // behind the window; its rounded mask also shapes the shadow.
        let effect = NSVisualEffectView(frame: NSRect(origin: .zero, size: size))
        effect.material = .popover
        effect.blendingMode = .behindWindow
        effect.state = .active
        effect.wantsLayer = true
        effect.layer?.cornerRadius = cornerRadius
        effect.layer?.cornerCurve = .continuous
        effect.layer?.masksToBounds = true
        effect.layer?.borderWidth = 0.5
        effect.layer?.borderColor = NSColor.separatorColor.cgColor
        host.frame = effect.bounds
        host.autoresizingMask = [.width, .height]
        effect.addSubview(host)
        panel.contentView = effect

        // Top right of the screen with the mouse, under the menu bar: where
        // macOS shows its own notifications.
        let screen = NSScreen.screens.first(where: { $0.frame.contains(NSEvent.mouseLocation) }) ?? NSScreen.main
        let visible = screen?.visibleFrame ?? NSRect(origin: .zero, size: size)
        let final = NSPoint(x: visible.maxX - size.width - margin, y: visible.maxY - size.height - margin)
        panel.setFrameOrigin(NSPoint(x: final.x + 24, y: final.y))
        panel.alphaValue = 0

        // orderFrontRegardless = visible WITHOUT becoming key: the user is
        // typing in another app and must not lose focus to a toast.
        panel.orderFrontRegardless()
        panel.invalidateShadow()
        NSAnimationContext.runAnimationGroup { ctx in
            ctx.duration = 0.22
            ctx.timingFunction = CAMediaTimingFunction(name: .easeOut)
            panel.animator().setFrameOrigin(final)
            panel.animator().alphaValue = 1
        }

        current = panel
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
            body: "Already on the list. Cmd+C a new selection, then press your hotkey again."
        )
    }

    /// Fires when the user presses the dict hotkey without re-copying.
    /// Detected via NSPasteboard.changeCount staying identical across
    /// two consecutive presses — see DictHotkeyManager.handleCarbonHotKey.
    static func showWorkflowHint(hotkey: String) {
        show(
            kind: .workflowHint,
            title: "Press Cmd+C first",
            body: "Without Accessibility, Dimmy reads your clipboard. Steps: select word, Cmd+C, \(hotkey)."
        )
    }

    static func dismissCurrent() {
        dismissCurrent(animated: true)
    }

    private static func dismissCurrent(animated: Bool) {
        dismissTimer?.invalidate()
        dismissTimer = nil
        guard let panel = current else { return }
        current = nil
        // orderOut, never close(): close() goes through the AppKit
        // transform-animation teardown that SIGSEGVs against an in-flight
        // QuartzCore transaction.
        guard animated else { panel.orderOut(nil); return }
        NSAnimationContext.runAnimationGroup({ ctx in
            ctx.duration = 0.18
            panel.animator().alphaValue = 0
        }, completionHandler: {
            Task { @MainActor in panel.orderOut(nil) }
        })
    }
}

/// Content of the banner: the app icon with a status badge, a bold title
/// and up to three lines of text. Adaptive colours only, so light and dark
/// follow the system.
private struct DictToastView: View {
    let kind: DictToastWindow.Kind
    let title: String
    let message: String

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            ZStack(alignment: .bottomTrailing) {
                Image(nsImage: NSApp.applicationIconImage)
                    .resizable()
                    .interpolation(.high)
                    .frame(width: 34, height: 34)
                Image(systemName: kind.badgeSymbol)
                    .font(.system(size: 8, weight: .heavy))
                    .foregroundStyle(.white)
                    .frame(width: 16, height: 16)
                    .background(Circle().fill(kind.accent))
                    .overlay(Circle().stroke(Color(nsColor: .windowBackgroundColor), lineWidth: 1.5))
                    .offset(x: 4, y: 4)
            }
            VStack(alignment: .leading, spacing: 2) {
                Text(title)
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundStyle(.primary)
                    .lineLimit(2)
                Text(message)
                    .font(.system(size: 13))
                    .foregroundStyle(.secondary)
                    .lineLimit(3)
            }
            .fixedSize(horizontal: false, vertical: true)
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 12)
        .frame(width: DictToastWindow.width, alignment: .leading)
        .contentShape(Rectangle())
        .onTapGesture {
            // Click-to-dismiss for impatient users.
            Task { @MainActor in DictToastWindow.dismissCurrent() }
        }
    }
}

import AppKit
import SwiftUI
import Combine

// MARK: - SwiftUI subtitle layer

/// Movie-subtitle styled view: white text + soft shadow on a fully
/// transparent background. FIFO of the last N chunks rendered as
/// "first  ·  second" so the on-screen text rolls instead of growing.
struct CaptionView: View {
    let text: String

    var body: some View {
        Text(text)
            .font(.system(size: 22, weight: .semibold, design: .default))
            .foregroundColor(.white)
            .multilineTextAlignment(.center)
            .lineLimit(2)
            .truncationMode(.head)
            .frame(maxWidth: .infinity)
            .shadow(color: .black.opacity(0.85), radius: 4, x: 0, y: 2)
            .shadow(color: .black.opacity(0.55), radius: 2, x: 0, y: 0)
            .padding(.horizontal, 24)
    }
}

// MARK: - Controller

/// Floating, transparent NSPanel that mirrors the Windows CaptionWindow:
/// bottom-centered, ~70 % screen width, FIFO of the last two chunks,
/// no background fill, no chrome. Driven by `stt_chunk` events the
/// Rust core emits when both `chunk_streaming_enabled = true` and the
/// active local backend is Parakeet.
@MainActor
final class CaptionWindowController {
    private static let visibleChunks = 2
    private static let widthFraction: CGFloat = 0.70
    private static let bottomMargin: CGFloat = 80      // gap above Dock
    private static let panelHeight: CGFloat = 110      // ~2 wrapped lines
    private static let minWidth: CGFloat = 480
    private static let maxWidth: CGFloat = 1200
    private static let hideDelay: TimeInterval = 1.2   // after is_final

    private var panel: NSPanel?
    private var hostingView: NSHostingView<CaptionView>?
    private var chunks: [String] = []
    private var hideWorkItem: DispatchWorkItem?

    init() {
        setupPanel()
    }

    private func setupPanel() {
        let panel = NSPanel(
            contentRect: NSRect(x: 0, y: 0, width: 800, height: Self.panelHeight),
            styleMask: [.borderless, .nonactivatingPanel],
            backing: .buffered,
            defer: false
        )
        panel.level = .statusBar                      // above floating pill
        panel.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary, .stationary]
        panel.backgroundColor = .clear
        panel.isOpaque = false
        panel.hasShadow = false
        panel.hidesOnDeactivate = false
        panel.titleVisibility = .hidden
        panel.titlebarAppearsTransparent = true
        panel.ignoresMouseEvents = true               // click-through

        let host = NSHostingView(rootView: CaptionView(text: ""))
        host.frame = panel.contentView?.bounds ?? .zero
        host.autoresizingMask = [.width, .height]
        panel.contentView = host

        self.panel = panel
        self.hostingView = host
    }

    /// Append a chunk delta to the FIFO and refresh the on-screen text.
    /// Empty / whitespace-only deltas are ignored — the Rust core can
    /// emit one when a window contained only silence.
    func push(delta: String) {
        let trimmed = delta.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }
        chunks.append(trimmed)
        if chunks.count > Self.visibleChunks {
            chunks.removeFirst(chunks.count - Self.visibleChunks)
        }
        let joined = chunks.joined(separator: "  ·  ")
        hostingView?.rootView = CaptionView(text: joined)
    }

    func reset() {
        chunks.removeAll()
        hostingView?.rootView = CaptionView(text: "")
    }

    /// Park bottom-center on the primary screen at ~70 % width.
    func reposition() {
        guard let panel, let screen = NSScreen.main else { return }
        let work = screen.visibleFrame
        let width = max(Self.minWidth, min(Self.maxWidth, work.width * Self.widthFraction))
        let x = work.minX + (work.width - width) / 2
        let y = work.minY + Self.bottomMargin
        panel.setFrame(NSRect(x: x, y: y, width: width, height: Self.panelHeight),
                       display: true)
    }

    func show() {
        cancelHide()
        reposition()
        panel?.orderFrontRegardless()
    }

    func hide() {
        cancelHide()
        panel?.orderOut(nil)
    }

    /// Schedule a delayed hide + reset — used after a final chunk so
    /// the user gets a last glance before the window disappears.
    func scheduleHide() {
        cancelHide()
        let work = DispatchWorkItem { [weak self] in
            self?.panel?.orderOut(nil)
            self?.reset()
        }
        hideWorkItem = work
        DispatchQueue.main.asyncAfter(deadline: .now() + Self.hideDelay, execute: work)
    }

    private func cancelHide() {
        hideWorkItem?.cancel()
        hideWorkItem = nil
    }
}

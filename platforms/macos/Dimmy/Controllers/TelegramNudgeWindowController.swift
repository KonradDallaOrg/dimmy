import AppKit
import SwiftUI

/// Bottom-right popup asking "Transcribe + recap this Telegram audio?".
/// Shown when a new voice note lands in Saved Messages and auto-process
/// is OFF. Primary "Transcribe + recap" accepts, secondary "Dismiss"
/// skips it. Mirror of `Views/TelegramNudgeWindow.xaml(.cs)` on Windows;
/// geometry + recipe copied from `CallNudgeWindowController` (360x112,
/// 16/60 px margins, 30 s auto-dismiss, non-activating status-bar panel).
@MainActor
final class TelegramNudgeWindowController {
    static let shared = TelegramNudgeWindowController()

    private static let cardWidth: CGFloat = 360
    private static let cardHeight: CGFloat = 112
    private static let bottomMargin: CGFloat = 60
    private static let rightMargin: CGFloat = 16
    private static let autoDismissSeconds: TimeInterval = 30

    private var panel: NSPanel?
    private var hostingView: NSHostingView<TelegramNudgeView>?
    private var viewModel: TelegramNudgeViewModel?
    private var dismissTimer: Timer?
    private var currentMsgId: Int32 = 0

    private init() {}

    // MARK: - Public API

    func show(msgId: Int32, filename: String) {
        ensurePanel()
        guard let vm = viewModel else { return }
        currentMsgId = msgId
        let name = filename.isEmpty ? "a voice note" : filename
        vm.update(
            title: "New audio from Telegram",
            body: "Transcribe and recap \(name)?")
        present()
    }

    func hide() {
        cancelDismissTimer()
        panel?.orderOut(nil)
    }

    // MARK: - Panel lifecycle

    private func ensurePanel() {
        if panel != nil { return }

        let vm = TelegramNudgeViewModel(
            onPrimary: { [weak self] in self?.onPrimary() },
            onSecondary: { [weak self] in self?.onSecondary() },
            onClose: { [weak self] in self?.onSecondary() })
        self.viewModel = vm

        let host = NSHostingView(rootView: TelegramNudgeView(viewModel: vm))
        host.translatesAutoresizingMaskIntoConstraints = false
        self.hostingView = host

        let panel = NSPanel(
            contentRect: NSRect(x: 0, y: 0, width: Self.cardWidth, height: Self.cardHeight),
            styleMask: [.borderless, .nonactivatingPanel, .fullSizeContentView],
            backing: .buffered,
            defer: false)
        panel.level = .statusBar
        panel.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary, .stationary]
        panel.isMovableByWindowBackground = true
        panel.backgroundColor = .clear
        panel.isOpaque = false
        panel.hasShadow = true
        panel.hidesOnDeactivate = false
        panel.titleVisibility = .hidden
        panel.titlebarAppearsTransparent = true
        panel.contentView = host
        host.frame = panel.contentView?.bounds ?? .zero
        host.autoresizingMask = [.width, .height]

        self.panel = panel
    }

    private func present() {
        guard let panel else { return }
        positionPanel(panel)
        panel.orderFrontRegardless()
        scheduleDismiss()
    }

    private func positionPanel(_ panel: NSPanel) {
        let screen = NSScreen.main ?? NSScreen.screens.first
        guard let visible = screen?.visibleFrame else { return }
        let originX = visible.maxX - Self.cardWidth - Self.rightMargin
        let originY = visible.minY + Self.bottomMargin
        panel.setFrame(
            NSRect(x: originX, y: originY, width: Self.cardWidth, height: Self.cardHeight),
            display: true)
    }

    // MARK: - Auto-dismiss

    private func scheduleDismiss() {
        cancelDismissTimer()
        dismissTimer = Timer.scheduledTimer(
            withTimeInterval: Self.autoDismissSeconds,
            repeats: false) { [weak self] _ in
            Task { @MainActor in self?.onSecondary() }
        }
    }

    private func cancelDismissTimer() {
        dismissTimer?.invalidate()
        dismissTimer = nil
    }

    // MARK: - Button handlers — route through the orchestrator

    private func onPrimary() {
        let msgId = currentMsgId
        hide()
        TelegramService.shared.acceptNudge(msgId: msgId)
    }

    private func onSecondary() {
        let msgId = currentMsgId
        hide()
        TelegramService.shared.dismissNudge(msgId: msgId)
    }
}

// MARK: - View model

@MainActor
final class TelegramNudgeViewModel: ObservableObject {
    @Published var titleText: String = ""
    @Published var bodyText: String = ""

    let onPrimary: () -> Void
    let onSecondary: () -> Void
    let onClose: () -> Void

    init(onPrimary: @escaping () -> Void,
         onSecondary: @escaping () -> Void,
         onClose: @escaping () -> Void) {
        self.onPrimary = onPrimary
        self.onSecondary = onSecondary
        self.onClose = onClose
    }

    func update(title: String, body: String) {
        self.titleText = title
        self.bodyText = body
    }
}

// MARK: - SwiftUI view

private struct TelegramNudgeView: View {
    @ObservedObject var viewModel: TelegramNudgeViewModel

    var body: some View {
        ZStack {
            TelegramNudgeVisualEffectBackground(material: .hudWindow, blendingMode: .behindWindow)
                .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
            VStack(alignment: .leading, spacing: 6) {
                HStack(spacing: 10) {
                    Image(systemName: "waveform.circle.fill")
                        .font(.system(size: 16, weight: .semibold))
                        .foregroundStyle(.primary)
                    Text(viewModel.titleText)
                        .font(.system(size: 13, weight: .semibold))
                        .lineLimit(1)
                        .truncationMode(.tail)
                    Spacer(minLength: 4)
                    Button {
                        viewModel.onClose()
                    } label: {
                        Image(systemName: "xmark")
                            .font(.system(size: 10, weight: .semibold))
                    }
                    .buttonStyle(.plain)
                    .frame(width: 18, height: 18)
                }
                Text(viewModel.bodyText)
                    .font(.system(size: 12))
                    .foregroundStyle(.secondary)
                    .lineLimit(2)
                Spacer(minLength: 0)
                HStack(spacing: 6) {
                    Spacer()
                    Button("Dismiss") { viewModel.onSecondary() }
                        .controlSize(.small)
                    Button("Transcribe + recap") { viewModel.onPrimary() }
                        .controlSize(.small)
                        .keyboardShortcut(.defaultAction)
                }
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 10)
        }
    }
}

private struct TelegramNudgeVisualEffectBackground: NSViewRepresentable {
    let material: NSVisualEffectView.Material
    let blendingMode: NSVisualEffectView.BlendingMode

    func makeNSView(context: Context) -> NSVisualEffectView {
        let view = NSVisualEffectView()
        view.material = material
        view.blendingMode = blendingMode
        view.state = .active
        view.isEmphasized = true
        return view
    }

    func updateNSView(_ view: NSVisualEffectView, context: Context) {
        view.material = material
        view.blendingMode = blendingMode
    }
}

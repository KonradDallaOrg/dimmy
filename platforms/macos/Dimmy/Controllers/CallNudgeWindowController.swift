import AppKit
import Combine
import SwiftUI

/// Teams-style bottom-right popup. Two modes:
///
///   • `.detected` — a call was just detected via the audio session
///     poll. Primary "Record now" starts a meeting. Secondary "Not now"
///     pushes per-app cooldown. Optional menu offers "Don't ask for
///     {app} again" → adds to exclusion list.
///
///   • `.stopSuggested` — both mic and sys-audio have been silent
///     past their thresholds while WE are meeting-recording. Primary
///     "Stop & recap" runs the recap pipeline. Secondary "Keep
///     recording" pushes a 5-min cooldown.
///
/// Mirror of `Views/CallNudgeWindow.xaml(.cs)` on Windows. Recipe and
/// geometry are deliberately copied — 360×112, 16/60 px margins from
/// the visible frame, 30 s auto-dismiss.
@MainActor
final class CallNudgeWindowController {
    enum Mode {
        case detected
        case stopSuggested
    }

    static let shared = CallNudgeWindowController()

    private static let cardWidth: CGFloat = 360
    private static let cardHeight: CGFloat = 112
    private static let bottomMargin: CGFloat = 60
    private static let rightMargin: CGFloat = 16
    private static let autoDismissSeconds: TimeInterval = 30

    /// Lowercase canonical id → user-facing app name. Mirror of
    /// CallNudgeWindow.xaml.cs:57-65.
    private static let appDisplayNames: [String: String] = [
        "teams": "Microsoft Teams",
        "zoom": "Zoom",
        "slack": "Slack",
        "discord": "Discord",
        "webex": "Cisco Webex",
    ]

    private var panel: NSPanel?
    private var hostingView: NSHostingView<CallNudgeView>?
    private var viewModel: CallNudgeViewModel?
    private var dismissTimer: Timer?
    /// True while WE set the panel frame, so the didMove observer doesn't
    /// persist a programmatic reposition as if the user had dragged it.
    private var isRepositioning = false
    /// UserDefaults key for the user-dragged origin (NSStringFromPoint).
    private static let savedOriginKey = "callNudgePanelOrigin"

    private init() {}

    // MARK: - Public API (Mode entry points)

    func showDetected(app: String?) {
        ensurePanel()
        guard let vm = viewModel else { return }
        vm.update(
            mode: .detected,
            app: app,
            displayName: Self.displayName(for: app),
            iconSystemName: "phone.bubble.left.fill",
            title: Self.detectedTitle(for: app),
            body: app == nil
                ? "Looks like a call. Record + recap with Dimmy?"
                : "Dimmy can record + recap this call.",
            primary: "Record now",
            secondary: "Not now")
        present()
    }

    func showStopSuggestion(app: String?) {
        ensurePanel()
        guard let vm = viewModel else { return }
        // Label the primary CTA from the recap flag pinned at meeting
        // start. The response string sent to Rust stays "stop_and_recap"
        // in both cases (it's just a "user accepted the stop" signal —
        // the actual recap-skip is decided downstream in
        // stopMeetingFromPill which reads the same flag). Win parity:
        // popup CTA relabels "Stop" / "Stop & recap".
        let withRecap = AppState.shared.meetingGenerateRecap
        vm.update(
            mode: .stopSuggested,
            app: app,
            displayName: Self.displayName(for: app),
            iconSystemName: "stop.circle.fill",
            title: Self.stopTitle(for: app),
            body: withRecap
                ? "No activity for a while. Stop & recap?"
                : "No activity for a while. Stop the meeting?",
            primary: withRecap ? "Stop & recap" : "Stop",
            secondary: "Keep recording")
        present()
    }

    func hide() {
        cancelDismissTimer()
        panel?.orderOut(nil)
    }

    // MARK: - Panel lifecycle

    private func ensurePanel() {
        if panel != nil { return }

        let vm = CallNudgeViewModel(
            onPrimary: { [weak self] in self?.onPrimary() },
            onSecondary: { [weak self] in self?.onSecondary() },
            onClose: { [weak self] in self?.onSecondary() },
            onNeverAskAgain: { [weak self] in self?.onNeverAskAgain() })
        self.viewModel = vm

        let rootView = CallNudgeView(viewModel: vm)
        let host = NSHostingView(rootView: rootView)
        host.translatesAutoresizingMaskIntoConstraints = false
        self.hostingView = host

        let panel = NSPanel(
            contentRect: NSRect(x: 0, y: 0, width: Self.cardWidth, height: Self.cardHeight),
            styleMask: [.borderless, .nonactivatingPanel, .fullSizeContentView],
            backing: .buffered,
            defer: false)
        // `.statusBar` sits above normal windows AND above macOS
        // notification banners, so the nudge is not buried under other
        // notifications (Marta's report: bottom-right card covered + missed).
        panel.level = .statusBar
        panel.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary, .stationary]
        // Draggable by its background so the user can move it out of the way
        // of whatever else lives in that corner; the position is remembered.
        panel.isMovableByWindowBackground = true
        panel.backgroundColor = .clear
        panel.isOpaque = false
        panel.hasShadow = true
        panel.hidesOnDeactivate = false
        panel.titleVisibility = .hidden
        panel.titlebarAppearsTransparent = true
        panel.acceptsMouseMovedEvents = true
        panel.contentView = host
        host.frame = panel.contentView?.bounds ?? .zero
        host.autoresizingMask = [.width, .height]

        self.panel = panel

        // Remember where the user drags it. Built once (ensurePanel guards on
        // panel != nil) so this observer is added exactly once — no leak.
        NotificationCenter.default.addObserver(
            forName: NSWindow.didMoveNotification, object: panel, queue: .main
        ) { [weak self] _ in
            guard let self, let p = self.panel, !self.isRepositioning else { return }
            UserDefaults.standard.set(NSStringFromPoint(p.frame.origin), forKey: Self.savedOriginKey)
        }
    }

    private func present() {
        guard let panel else { return }
        positionPanel(panel)
        panel.orderFrontRegardless()
        scheduleDismiss()
    }

    /// Restore the user's dragged position if it still lands on a visible
    /// screen; otherwise fall back to the bottom-right default.
    private func positionPanel(_ panel: NSPanel) {
        isRepositioning = true
        defer { isRepositioning = false }

        let size = NSSize(width: Self.cardWidth, height: Self.cardHeight)
        if let saved = UserDefaults.standard.string(forKey: Self.savedOriginKey) {
            let origin = NSPointFromString(saved)
            let rect = NSRect(origin: origin, size: size)
            // Only honour it if the card is (mostly) on some visible screen —
            // a stale origin from an unplugged monitor must not hide it.
            if NSScreen.screens.contains(where: { $0.visibleFrame.intersects(rect) }) {
                panel.setFrame(rect, display: true)
                return
            }
        }

        let screen = NSScreen.main ?? NSScreen.screens.first
        guard let visible = screen?.visibleFrame else { return }
        let originX = visible.maxX - Self.cardWidth - Self.rightMargin
        let originY = visible.minY + Self.bottomMargin
        panel.setFrame(NSRect(x: originX, y: originY, width: size.width, height: size.height),
                       display: true)
    }

    // MARK: - Auto-dismiss

    private func scheduleDismiss() {
        cancelDismissTimer()
        let timer = Timer.scheduledTimer(
            withTimeInterval: Self.autoDismissSeconds,
            repeats: false) { [weak self] _ in
            Task { @MainActor in self?.onTimeout() }
        }
        dismissTimer = timer
    }

    private func cancelDismissTimer() {
        dismissTimer?.invalidate()
        dismissTimer = nil
    }

    // MARK: - Button handlers — route via AppState so the central
    // ‘record now → start meeting’ flow stays in one place.

    private func onPrimary() {
        guard let vm = viewModel else { hide(); return }
        let app = vm.app
        let mode = vm.mode
        hide()
        AppState.shared.callNudgeRespond(
            app: app,
            response: mode == .detected ? "record_now" : "stop_and_recap")
    }

    private func onSecondary() {
        guard let vm = viewModel else { hide(); return }
        let app = vm.app
        let mode = vm.mode
        hide()
        AppState.shared.callNudgeRespond(
            app: app,
            response: mode == .detected ? "not_now" : "keep_recording")
    }

    private func onTimeout() {
        guard let vm = viewModel else { hide(); return }
        let app = vm.app
        let mode = vm.mode
        hide()
        AppState.shared.callNudgeRespond(
            app: app,
            response: mode == .detected ? "timeout" : "stop_timeout")
    }

    private func onNeverAskAgain() {
        guard let vm = viewModel else { hide(); return }
        let app = vm.app
        hide()
        AppState.shared.callNudgeRespond(app: app, response: "never")
    }

    // MARK: - Display-name helpers

    private static func displayName(for app: String?) -> String? {
        guard let app else { return nil }
        return appDisplayNames[app.lowercased()] ?? app.capitalized
    }

    private static func detectedTitle(for app: String?) -> String {
        if let name = displayName(for: app) {
            return "Meeting detected in \(name)"
        }
        return "Microphone in use"
    }

    private static func stopTitle(for app: String?) -> String {
        if let name = displayName(for: app) {
            return "\(name) call ended?"
        }
        return "Call ended?"
    }
}

// MARK: - View model

@MainActor
final class CallNudgeViewModel: ObservableObject {
    @Published var mode: CallNudgeWindowController.Mode = .detected
    @Published var app: String?
    @Published var displayName: String?
    @Published var iconSystemName: String = "phone.bubble.left.fill"
    @Published var titleText: String = ""
    @Published var bodyText: String = ""
    @Published var primaryLabel: String = "Record now"
    @Published var secondaryLabel: String = "Not now"

    let onPrimary: () -> Void
    let onSecondary: () -> Void
    let onClose: () -> Void
    let onNeverAskAgain: () -> Void

    init(onPrimary: @escaping () -> Void,
         onSecondary: @escaping () -> Void,
         onClose: @escaping () -> Void,
         onNeverAskAgain: @escaping () -> Void) {
        self.onPrimary = onPrimary
        self.onSecondary = onSecondary
        self.onClose = onClose
        self.onNeverAskAgain = onNeverAskAgain
    }

    func update(mode: CallNudgeWindowController.Mode,
                app: String?,
                displayName: String?,
                iconSystemName: String,
                title: String,
                body: String,
                primary: String,
                secondary: String) {
        self.mode = mode
        self.app = app
        self.displayName = displayName
        self.iconSystemName = iconSystemName
        self.titleText = title
        self.bodyText = body
        self.primaryLabel = primary
        self.secondaryLabel = secondary
    }
}

// MARK: - SwiftUI view

private struct CallNudgeView: View {
    @ObservedObject var viewModel: CallNudgeViewModel

    var body: some View {
        ZStack {
            NudgeVisualEffectBackground(material: .hudWindow, blendingMode: .behindWindow)
                .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
            VStack(alignment: .leading, spacing: 6) {
                HStack(spacing: 10) {
                    Image(systemName: viewModel.iconSystemName)
                        .font(.system(size: 16, weight: .semibold))
                        .foregroundStyle(.primary)
                    Text(viewModel.titleText)
                        .font(.system(size: 13, weight: .semibold))
                        .lineLimit(1)
                        .truncationMode(.tail)
                    Spacer(minLength: 4)
                    if viewModel.mode == .detected, viewModel.app != nil {
                        Menu {
                            Button("Don't ask for \(viewModel.displayName ?? "this app") again") {
                                viewModel.onNeverAskAgain()
                            }
                        } label: {
                            Image(systemName: "ellipsis")
                        }
                        .menuStyle(.borderlessButton)
                        .menuIndicator(.hidden)
                        .fixedSize()
                        .frame(width: 18, height: 18)
                    }
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
                    Button(viewModel.secondaryLabel) {
                        viewModel.onSecondary()
                    }
                    .controlSize(.small)
                    Button(viewModel.primaryLabel) {
                        viewModel.onPrimary()
                    }
                    .controlSize(.small)
                    .keyboardShortcut(.defaultAction)
                }
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 10)
        }
    }
}

private struct NudgeVisualEffectBackground: NSViewRepresentable {
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

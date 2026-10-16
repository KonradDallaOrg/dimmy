import AppKit
import AVFoundation
import IOKit.hid
import SwiftUI

/// Small floating window listing the permissions Dimmy needs, each with its
/// live state and a button to the exact System Settings pane.
///
/// Shown instead of the full onboarding when an AI assistant already set
/// Dimmy up from the terminal (`Dimmy --setup`): everything is configured
/// except what only the user can grant. Also opened by
/// `Dimmy --setup perm guide`, through the `.show-permission-guide` trigger
/// file in the config dir.
@MainActor
final class PermissionGuideWindowController: NSObject, NSWindowDelegate {
    static let shared = PermissionGuideWindowController()

    static let triggerFileName = ".show-permission-guide"

    private var window: NSWindow?

    private override init() { super.init() }

    func show() {
        if let existing = window {
            existing.makeKeyAndOrderFront(nil)
            NSApp.activate(ignoringOtherApps: true)
            return
        }
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 460, height: 420),
            styleMask: [.titled, .closable, .fullSizeContentView],
            backing: .buffered,
            defer: false
        )
        window.title = "Permissions"
        window.titlebarAppearsTransparent = true
        window.isReleasedWhenClosed = false
        window.level = .floating
        window.delegate = self
        let finish: () -> Void = { [weak window] in window?.performClose(nil) }
        window.contentView = NSHostingView(
            rootView: PermissionGuideView(permissions: PermissionsManager.shared, onDone: finish)
        )
        window.center()
        self.window = window
        window.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
    }

    /// Shows the guide when `Dimmy --setup perm guide` asked for it, then
    /// removes the request. Called at launch and whenever the app becomes
    /// active (the CLI runs `open` on the app, which activates it).
    func showIfRequested() {
        guard let dir = DimmyCore.shared.configDirURL else { return }
        let trigger = dir.appendingPathComponent(Self.triggerFileName)
        guard FileManager.default.fileExists(atPath: trigger.path) else { return }
        try? FileManager.default.removeItem(at: trigger)
        show()
    }

    func windowWillClose(_ notification: Notification) {
        // Dropping the window drops the view, and with it the refresh timer.
        window?.contentView = nil
        window = nil
    }
}

private enum GuidePermission: String, CaseIterable, Identifiable {
    case microphone
    case accessibility
    case inputMonitoring = "input_monitoring"
    case systemAudio = "system_audio"

    var id: String { rawValue }

    var title: String {
        switch self {
        case .microphone: return "Microphone"
        case .accessibility: return "Accessibility"
        case .inputMonitoring: return "Input Monitoring"
        case .systemAudio: return "Screen & System Audio Recording"
        }
    }

    var purpose: String {
        switch self {
        case .microphone: return "Hear you when you dictate or record a meeting."
        case .accessibility: return "Listen for your shortcut and paste the text."
        case .inputMonitoring: return "Needed on some Macs for the shortcut. Optional."
        case .systemAudio: return "Record the other side of a call. Optional."
        }
    }

    var icon: String {
        switch self {
        case .microphone: return "mic.fill"
        case .accessibility: return "accessibility"
        case .inputMonitoring: return "keyboard"
        case .systemAudio: return "speaker.wave.2.fill"
        }
    }

    var required: Bool { self == .microphone || self == .accessibility }

    var settingsURL: URL? {
        let pane: String
        switch self {
        case .microphone: pane = "Privacy_Microphone"
        case .accessibility: pane = "Privacy_Accessibility"
        case .inputMonitoring: pane = "Privacy_ListenEvent"
        case .systemAudio: pane = "Privacy_ScreenCapture"
        }
        return URL(string: "x-apple.systempreferences:com.apple.preference.security?\(pane)")
    }

    /// `PermissionGuide/<permission>-macos<major>` in the asset catalog.
    /// None ship yet; the row simply has no picture until one does.
    var referenceImage: NSImage? {
        let major = ProcessInfo.processInfo.operatingSystemVersion.majorVersion
        return NSImage(named: "PermissionGuide/\(rawValue)-macos\(major)")
    }
}

private struct PermissionGuideView: View {
    @ObservedObject var permissions: PermissionsManager
    let onDone: () -> Void

    /// TCC posts no notification when a grant changes, so while this window
    /// is open, and only then, the state is re-read every second. The timer
    /// starts on appear and stops on disappear (the window drops its view on
    /// close). Same documented exception as the TCC reads in
    /// PermissionsManager; see "No FFI-state polling rule" in CLAUDE.md.
    @State private var timer: Timer?

    private var appName: String {
        (Bundle.main.object(forInfoDictionaryKey: "CFBundleDisplayName") as? String)
            ?? (Bundle.main.object(forInfoDictionaryKey: "CFBundleName") as? String)
            ?? "Dimmy"
    }

    private var allRequiredGranted: Bool { permissions.allRequiredGranted }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Text("Permissions")
                .font(.system(size: 20, weight: .semibold))
                .padding(.top, 8)
            Text("Your assistant set up \(appName). macOS asks you, not the assistant, for these.")
                .font(.system(size: 12))
                .foregroundStyle(Color.macTextSecondary)
                .fixedSize(horizontal: false, vertical: true)
                .padding(.top, 4)

            MacGroupLabel(text: "Needed")
            MacTile {
                row(.microphone, last: false)
                row(.accessibility, last: true)
            }
            MacGroupLabel(text: "Optional")
            MacTile {
                row(.inputMonitoring, last: false)
                row(.systemAudio, last: true)
            }

            Spacer(minLength: 16)

            HStack {
                if allRequiredGranted {
                    MacStatusPill(text: "All set", dotColor: .green)
                } else {
                    MacStatusPill(text: "Waiting for you", dotColor: .orange)
                }
                Spacer()
                Button(allRequiredGranted ? "Done" : "Later", action: onDone)
                    .keyboardShortcut(.defaultAction)
            }
        }
        .padding(.horizontal, MacTheme.pageHorizontalPadding)
        .padding(.top, 20)
        .padding(.bottom, 20)
        .frame(width: 460)
        .onAppear {
            permissions.refreshOffMain()
            timer = Timer.scheduledTimer(withTimeInterval: 1.0, repeats: true) { _ in
                Task { @MainActor in PermissionsManager.shared.refreshOffMain() }
            }
        }
        .onDisappear {
            timer?.invalidate()
            timer = nil
        }
    }

    private func state(_ p: GuidePermission) -> (granted: Bool, label: String) {
        switch p {
        case .microphone:
            switch permissions.microphone {
            case .authorized: return (true, "On")
            case .notDetermined: return (false, "Not asked yet")
            default: return (false, "Off")
            }
        case .accessibility:
            return permissions.accessibility ? (true, "On") : (false, "Off")
        case .inputMonitoring:
            return permissions.inputMonitoringGranted ? (true, "On") : (false, "Off")
        case .systemAudio:
            switch permissions.systemAudio {
            case "granted": return (true, "On")
            case "denied": return (false, "Off")
            default: return (false, "Checked during a meeting")
            }
        }
    }

    @ViewBuilder
    private func row(_ p: GuidePermission, last: Bool) -> some View {
        let s = state(p)
        VStack(alignment: .leading, spacing: 0) {
            MacRow(p.title,
                   description: s.granted ? p.purpose : "\(p.purpose) Turn on \(appName) in this list.",
                   icon: p.icon,
                   iconBackground: s.granted ? .green : Color.gray.opacity(0.6),
                   showsDivider: last ? false : (p.referenceImage == nil)) {
                HStack(spacing: 8) {
                    Image(systemName: s.granted ? "checkmark.circle.fill" : "circle")
                        .foregroundStyle(s.granted ? Color.green : Color.macTextTertiary)
                        .help(s.label)
                    if !s.granted {
                        Button(p == .microphone && permissions.microphone == .notDetermined
                               ? "Allow" : "Open Settings") {
                            openPane(p)
                        }
                        .controlSize(.small)
                    }
                }
            }
            if !s.granted, let image = p.referenceImage {
                Image(nsImage: image)
                    .resizable()
                    .aspectRatio(contentMode: .fit)
                    .frame(maxHeight: 140)
                    .clipShape(RoundedRectangle(cornerRadius: 8, style: .continuous))
                    .padding(.horizontal, MacTheme.rowHorizontalPadding)
                    .padding(.bottom, 10)
            }
        }
    }

    /// Asking first puts Dimmy in the list the pane shows, so "turn on
    /// Dimmy in this list" is true when the pane opens.
    private func openPane(_ p: GuidePermission) {
        switch p {
        case .microphone:
            if permissions.microphone == .notDetermined {
                Task { _ = await permissions.requestMicrophone() }
                return
            }
        case .accessibility:
            permissions.promptAccessibility()
        case .inputMonitoring:
            permissions.requestInputMonitoring()
        case .systemAudio:
            break
        }
        if let url = p.settingsURL {
            NSWorkspace.shared.open(url)
        }
    }
}

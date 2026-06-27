import AVFoundation
import SwiftUI

// Permissions, mirrors the onboarding step's logic but rendered in the
// Tahoe Settings shell so users can review/grant access at any time
// without re-running onboarding. PermissionsManager is the source of
// truth; we just bind to its @Published flags and surface CTAs.

struct MacPermissionsPage: View {
    @ObservedObject var appState: AppState
    @ObservedObject private var perms = PermissionsManager.shared

    @State private var micRequestInFlight = false
    @State private var accessibilityPromptShown = false
    @State private var inputMonitoringPromptShown = false

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            statusGroup
            permissionsGroup

            if accessibilityPromptShown && !perms.accessibilityGranted {
                resetGroup
            }

            if appState.showAdvanced {
                diagnosticsGroup
            }
        }
        .onAppear { perms.refresh() }
    }

    // MARK: Status banner

    private var statusGroup: some View {
        Group {
            MacGroupLabel(text: "Status")
            MacTile {
                MacRow(
                    perms.allRequiredGranted ? "All set"
                                             : "Some access still missing",
                    description: perms.allRequiredGranted
                        ? "Dimmy has everything it needs to record and paste."
                        : "The shortcut and paste-back won't work until both are granted.",
                    showsDivider: false
                ) {
                    Image(systemName: perms.allRequiredGranted
                          ? "checkmark.seal.fill"
                          : "exclamationmark.triangle.fill")
                        .font(.system(size: 18))
                        .foregroundStyle(perms.allRequiredGranted ? .green : .orange)
                }
            }
        }
    }

    // MARK: Permission rows

    private var permissionsGroup: some View {
        Group {
            MacGroupLabel(text: "Required access")
            MacTile {
                permissionRow(
                    icon: "mic.fill",
                    iconBg: Color(red: 1.00, green: 0.22, blue: 0.37),
                    title: "Microphone",
                    description: "Record your voice",
                    granted: perms.microphoneGranted,
                    pending: perms.microphone == .notDetermined,
                    showsDivider: true,
                    action: requestMic,
                    openSystemSettings: openMicSettings
                )

                permissionRow(
                    icon: "hand.raised.fill",
                    iconBg: Color(red: 0.04, green: 0.52, blue: 1.00),
                    title: "Accessibility",
                    description: "Hotkey + paste back.",
                    hint: "Lets Dimmy listen for the global hotkey from any app and paste the transcript back into the focused field. Without it the shortcut is dead and the transcript stays on the clipboard only.",
                    granted: perms.accessibilityGranted,
                    pending: !perms.accessibilityGranted && !accessibilityPromptShown,
                    showsDivider: true,
                    action: requestAccessibility,
                    openSystemSettings: perms.openAccessibilitySettings
                )

                // Always shown in settings so users can audit status, even if
                // their current shortcut doesn't require Input Monitoring (the
                // Fn key is the only one that does, but they may switch later).
                permissionRow(
                    icon: "keyboard",
                    iconBg: Color(red: 0.20, green: 0.78, blue: 0.35),
                    title: "Input Monitoring",
                    description: appState.shortcut.isFnOnly
                        ? "Required by your Fn-key shortcut."
                        : "Only needed for Fn-key shortcuts.",
                    hint: "macOS treats the Fn / Globe key as a HID device, not a regular modifier. Dimmy needs Input Monitoring to see Fn-based combos. Cmd/Ctrl/Opt/Shift combos work without it.",
                    granted: perms.inputMonitoringGranted,
                    pending: perms.inputMonitoring == kIOHIDAccessTypeUnknown && !inputMonitoringPromptShown,
                    showsDivider: false,
                    action: requestInputMonitoring,
                    openSystemSettings: perms.openInputMonitoringSettings
                )
            }
            MacGroupFooter(text: "Permissions are managed by macOS in System Settings → Privacy & Security. You can revoke them there at any time.")
        }
    }

    // MARK: Stale-TCC reset (shown only if user clicked Grant but macOS still doesn't see access)

    private var resetGroup: some View {
        Group {
            MacGroupLabel(text: "Stuck?")
            MacTile {
                VStack(alignment: .leading, spacing: 8) {
                    Text("Dimmy looks enabled in System Settings but still isn't detected?")
                        .font(.system(size: 13, weight: .medium))
                    Text("macOS may have kept a stale entry from a previous build. Reset Dimmy's Accessibility grant and try again.")
                        .font(.system(size: 11))
                        .foregroundStyle(Color.macTextSecondary)
                        .fixedSize(horizontal: false, vertical: true)
                    HStack {
                        Spacer()
                        Button("Reset and re-grant Accessibility") {
                            perms.resetTccEntries(services: ["Accessibility"])
                            accessibilityPromptShown = false
                        }
                        .controlSize(.small)
                    }
                }
                .padding(EdgeInsets(top: 12, leading: 14, bottom: 12, trailing: 14))
            }
        }
    }

    // MARK: Row builder

    private func permissionRow(
        icon: String,
        iconBg: Color,
        title: String,
        description: String,
        hint: String? = nil,
        granted: Bool,
        pending: Bool,
        showsDivider: Bool,
        action: @escaping () -> Void,
        openSystemSettings: @escaping () -> Void
    ) -> some View {
        MacRow(
            title,
            description: description,
            hint: hint,
            icon: icon,
            iconBackground: iconBg,
            showsDivider: showsDivider
        ) {
            if granted {
                HStack(spacing: 4) {
                    Image(systemName: "checkmark.circle.fill")
                        .foregroundStyle(.green)
                    Text("Granted")
                        .font(.system(size: 12, weight: .medium))
                        .foregroundStyle(.green)
                }
                Button {
                    openSystemSettings()
                } label: {
                    Image(systemName: "arrow.up.right.square")
                }
                .buttonStyle(.plain)
                .help("Open in System Settings")
            } else {
                Button(pending ? "Grant" : "Open System Settings", action: action)
                    .controlSize(.small)
            }
        }
    }

    // MARK: Diagnostics (Advanced), exposes raw TCC values so we can tell
    // when a "granted" reading is actually noise (e.g. on cloud/RDP Macs
    // without a real audio device, AVCaptureDevice can report .authorized
    // even though TCC has no entry for the bundle).

    private var diagnosticsGroup: some View {
        Group {
            MacGroupLabel(text: "Diagnostics")
            MacTile {
                MacRow("Microphone status", showsDivider: true) {
                    Text(micStatusRaw)
                        .font(.system(size: 12, design: .monospaced))
                        .foregroundStyle(Color.macTextSecondary)
                }
                MacRow("Accessibility status", showsDivider: true) {
                    Text(perms.accessibilityGranted ? "trusted" : "untrusted")
                        .font(.system(size: 12, design: .monospaced))
                        .foregroundStyle(Color.macTextSecondary)
                }
                MacRow("Input Monitoring status", showsDivider: true) {
                    Text(inputMonitoringRaw)
                        .font(.system(size: 12, design: .monospaced))
                        .foregroundStyle(Color.macTextSecondary)
                }
                MacRow(
                    "Reset and re-prompt",
                    hint: "Wipes Dimmy's TCC entry for the chosen permission. The reset is immediate — for System Audio just start a new meeting and the tap re-requests the grant (NO relaunch needed). Only the Accessibility status checkmark above can stay stale until relaunch (macOS latches it per-process) — that's cosmetic, not functional. (\"System Audio\" = the meeting tap, kTCCServiceAudioCapture.)",
                    showsDivider: false
                ) {
                    Button("Reset Microphone") {
                        perms.resetTccEntries(services: ["Microphone"])
                    }
                    .controlSize(.small)
                    Button("Reset Accessibility") {
                        perms.resetTccEntries(services: ["Accessibility"])
                    }
                    .controlSize(.small)
                    Button("Reset System Audio") {
                        perms.resetTccEntries(services: ["AudioCapture"])
                    }
                    .controlSize(.small)
                    // Optional: only needed to refresh the Accessibility checkmark
                    // (latched per-process). NOT needed for the audio reset to work.
                    Button("Relaunch (refresh status)") {
                        perms.relaunchApp()
                    }
                    .controlSize(.small)
                }
            }
        }
    }

    private var micStatusRaw: String {
        switch perms.microphone {
        case .authorized:    return "authorized"
        case .denied:        return "denied"
        case .notDetermined: return "notDetermined"
        case .restricted:    return "restricted"
        @unknown default:    return "unknown(\(perms.microphone.rawValue))"
        }
    }

    private var inputMonitoringRaw: String {
        switch perms.inputMonitoring {
        case kIOHIDAccessTypeGranted: return "granted"
        case kIOHIDAccessTypeDenied:  return "denied"
        case kIOHIDAccessTypeUnknown: return "unknown"
        default:                       return "raw(\(perms.inputMonitoring.rawValue))"
        }
    }

    /// Microphone deep link, Privacy & Security → Microphone.
    private func openMicSettings() {
        if let url = URL(string: "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone") {
            NSWorkspace.shared.open(url)
        }
    }

    // MARK: Actions

    private func requestMic() {
        guard !micRequestInFlight else { return }
        micRequestInFlight = true
        Task { @MainActor in
            if perms.microphone == .notDetermined {
                _ = await perms.requestMicrophone()
            } else if let url = URL(string: "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone") {
                NSWorkspace.shared.open(url)
            }
            micRequestInFlight = false
            perms.refreshNow()
        }
    }

    private func requestAccessibility() {
        if perms.accessibilityGranted { return }
        if accessibilityPromptShown {
            perms.openAccessibilitySettings()
        } else {
            perms.promptAccessibility()
            withAnimation { accessibilityPromptShown = true }
        }
        perms.refreshNow()
    }

    private func requestInputMonitoring() {
        if perms.inputMonitoringGranted { return }
        if inputMonitoringPromptShown {
            perms.openInputMonitoringSettings()
        } else {
            perms.requestInputMonitoring()
            withAnimation { inputMonitoringPromptShown = true }
        }
        perms.refreshNow()
    }
}

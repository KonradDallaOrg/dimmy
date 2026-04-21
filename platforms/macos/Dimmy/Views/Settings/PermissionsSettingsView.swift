import AVFoundation
import SwiftUI

struct PermissionsSettingsView: View {
    @ObservedObject var appState: AppState
    @ObservedObject private var perms = PermissionsManager.shared

    var body: some View {
        Form {
            Section("Required Permissions") {
                row(
                    icon: "mic.fill",
                    title: "Microphone",
                    description: "Required to capture your voice",
                    granted: perms.microphoneGranted,
                    openAction: { openSystemSettings("Privacy_Microphone") }
                )
                row(
                    icon: "hand.raised.fill",
                    title: "Accessibility",
                    description: "Required to paste text into the active app",
                    granted: perms.accessibilityGranted,
                    openAction: perms.openAccessibilitySettings
                )
                row(
                    icon: "keyboard",
                    title: "Input Monitoring",
                    description: "Required to listen for your global shortcut",
                    granted: perms.inputMonitoringGranted,
                    openAction: perms.openInputMonitoringSettings
                )
            }

            Section {
                Button("Refresh") { perms.refresh() }
                Text("Status refreshes automatically when you return from System Settings.")
                    .font(.system(size: 11))
                    .foregroundColor(.secondary)
            }
        }
        .formStyle(.grouped)
        .onAppear { perms.refresh() }
    }

    private func row(
        icon: String,
        title: String,
        description: String,
        granted: Bool,
        openAction: @escaping () -> Void
    ) -> some View {
        HStack {
            Label {
                VStack(alignment: .leading, spacing: 2) {
                    Text(title).font(.system(size: 13, weight: .medium))
                    Text(description).font(.system(size: 11)).foregroundColor(.secondary)
                }
            } icon: {
                Image(systemName: icon)
                    .foregroundColor(granted ? .green : .orange)
            }
            Spacer()
            if granted {
                Image(systemName: "checkmark.circle.fill")
                    .foregroundColor(.green)
                    .font(.system(size: 16))
            } else {
                Button("Open") { openAction() }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
            }
        }
    }

    private func openSystemSettings(_ pane: String) {
        guard let url = URL(string: "x-apple.systempreferences:com.apple.preference.security?\(pane)") else { return }
        NSWorkspace.shared.open(url)
    }
}

import SwiftUI
import AVFoundation

struct PermissionsSettingsView: View {
    @ObservedObject var appState: AppState
    @State private var micGranted = false
    @State private var accessibilityGranted = false
    @State private var pollTimer: Timer?

    var body: some View {
        Form {
            Section("Required Permissions") {
                permissionRow(
                    icon: "mic.fill",
                    title: "Microphone",
                    description: "Required to capture your voice",
                    granted: micGranted
                )

                permissionRow(
                    icon: "hand.raised.fill",
                    title: "Accessibility",
                    description: "Required to paste text in active apps",
                    granted: accessibilityGranted
                )
            }

            Section {
                Button("Open System Settings") {
                    let url = URL(string: "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")!
                    NSWorkspace.shared.open(url)
                }

                Text("If a permission is missing, open System Settings and toggle Dimmy ON")
                    .font(.system(size: 11))
                    .foregroundColor(.secondary)
            }
        }
        .formStyle(.grouped)
        .onAppear {
            checkPermissions()
            pollTimer = Timer.scheduledTimer(withTimeInterval: 1.0, repeats: true) { _ in
                Task { @MainActor in checkPermissions() }
            }
        }
        .onDisappear {
            pollTimer?.invalidate()
        }
    }

    private func permissionRow(icon: String, title: String, description: String, granted: Bool) -> some View {
        HStack {
            Label {
                VStack(alignment: .leading, spacing: 2) {
                    Text(title)
                        .font(.system(size: 13, weight: .medium))
                    Text(description)
                        .font(.system(size: 11))
                        .foregroundColor(.secondary)
                }
            } icon: {
                Image(systemName: icon)
                    .foregroundColor(granted ? .green : .orange)
            }

            Spacer()

            Image(systemName: granted ? "checkmark.circle.fill" : "xmark.circle")
                .foregroundColor(granted ? .green : .orange)
                .font(.system(size: 16))
        }
    }

    private func checkPermissions() {
        if AppState.skipPermissions {
            micGranted = true
            accessibilityGranted = true
            return
        }
        micGranted = AVCaptureDevice.authorizationStatus(for: .audio) == .authorized
        accessibilityGranted = AXIsProcessTrusted()
    }
}

import AVFoundation
import SwiftUI

struct PermissionsStepView: View {
    @ObservedObject var appState: AppState
    @ObservedObject private var perms = PermissionsManager.shared
    let onContinue: () -> Void

    @State private var micRequestInFlight = false
    @State private var accessibilityPromptShown = false
    @State private var inputMonitoringPromptShown = false

    var body: some View {
        ScrollView(.vertical, showsIndicators: false) {
        VStack(spacing: 16) {
            Text("Permissions")
                .font(.system(size: 26, weight: .bold))

            Text("Dimmy needs three permissions to record, listen for your shortcut, and paste text.")
                .font(.system(size: 13))
                .foregroundColor(.secondary)
                .multilineTextAlignment(.center)
                .padding(.horizontal, 24)

            VStack(spacing: 12) {
                permissionRow(
                    icon: "mic.fill",
                    title: "Microphone",
                    description: "Record your voice",
                    granted: perms.microphoneGranted,
                    pending: perms.microphone == .notDetermined,
                    action: requestMic
                )

                permissionRow(
                    icon: "hand.raised.fill",
                    title: "Accessibility",
                    description: "Paste text into active apps",
                    granted: perms.accessibilityGranted,
                    pending: !perms.accessibilityGranted && !accessibilityPromptShown,
                    action: requestAccessibility
                )

                permissionRow(
                    icon: "keyboard",
                    title: "Input Monitoring",
                    description: "Listen for your global shortcut",
                    granted: perms.inputMonitoringGranted,
                    pending: perms.inputMonitoring == kIOHIDAccessTypeUnknown && !inputMonitoringPromptShown,
                    action: requestInputMonitoring
                )
            }
            .padding(.horizontal, 20)

            if accessibilityPromptShown && !perms.accessibilityGranted {
                hintBanner(
                    icon: "arrow.up.right.square",
                    color: .orange,
                    text: "Toggle **Dimmy** ON in System Settings → Privacy & Security → Accessibility"
                )
            }
            if inputMonitoringPromptShown && !perms.inputMonitoringGranted {
                hintBanner(
                    icon: "arrow.up.right.square",
                    color: .orange,
                    text: "Toggle **Dimmy** ON in System Settings → Privacy & Security → Input Monitoring"
                )
            }

            Button(action: onContinue) {
                Text(perms.allRequiredGranted ? "Continue" : "Continue anyway")
                    .font(.system(size: 15, weight: .semibold))
                    .frame(maxWidth: 220)
            }
            .buttonStyle(.borderedProminent)
            .controlSize(.large)
            .disabled(!perms.microphoneGranted)
            .padding(.top, 4)

            if !perms.microphoneGranted {
                Text("Microphone is required — grant it to continue.")
                    .font(.system(size: 11))
                    .foregroundColor(Color(nsColor: .tertiaryLabelColor))
            } else if !perms.allRequiredGranted {
                Text("Accessibility can be granted later, but the global shortcut won't work without it.")
                    .font(.system(size: 11))
                    .foregroundColor(Color(nsColor: .tertiaryLabelColor))
                    .multilineTextAlignment(.center)
                    .padding(.horizontal, 24)
            }

            Spacer().frame(height: 8)
        }
        .padding(.horizontal, 28)
        .padding(.vertical, 16)
        }
        .onAppear { perms.refresh() }
    }

    // MARK: - Rows / banners

    private func permissionRow(
        icon: String,
        title: String,
        description: String,
        granted: Bool,
        pending: Bool,
        action: @escaping () -> Void
    ) -> some View {
        HStack(spacing: 14) {
            Image(systemName: icon)
                .font(.system(size: 22))
                .foregroundColor(granted ? .green : .accentColor)
                .frame(width: 34)

            VStack(alignment: .leading, spacing: 3) {
                Text(title)
                    .font(.system(size: 14, weight: .semibold))
                Text(description)
                    .font(.system(size: 12))
                    .foregroundColor(.secondary)
            }

            Spacer()

            if granted {
                Image(systemName: "checkmark.circle.fill")
                    .font(.system(size: 22))
                    .foregroundColor(.green)
                    .transition(.scale.combined(with: .opacity))
            } else {
                Button(pending ? "Grant" : "Open Settings") {
                    action()
                }
                .buttonStyle(.bordered)
                .controlSize(.regular)
            }
        }
        .padding(14)
        .background(
            RoundedRectangle(cornerRadius: 12)
                .fill(Color(nsColor: .controlBackgroundColor))
        )
        .animation(.easeInOut(duration: 0.3), value: granted)
    }

    private func hintBanner(icon: String, color: Color, text: String) -> some View {
        HStack(spacing: 10) {
            Image(systemName: icon)
                .foregroundColor(color)
                .font(.system(size: 16))
            Text(.init(text))
                .font(.system(size: 12))
                .foregroundColor(.secondary)
        }
        .padding(10)
        .background(
            RoundedRectangle(cornerRadius: 8)
                .fill(color.opacity(0.1))
        )
        .padding(.horizontal, 20)
        .transition(.opacity.combined(with: .move(edge: .top)))
    }

    // MARK: - Actions

    private func requestMic() {
        guard !micRequestInFlight else { return }
        micRequestInFlight = true
        Task { @MainActor in
            if perms.microphone == .notDetermined {
                _ = await perms.requestMicrophone()
            } else {
                // Already denied — user must toggle manually.
                if let url = URL(string: "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone") {
                    NSWorkspace.shared.open(url)
                }
            }
            micRequestInFlight = false
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
    }

    private func requestInputMonitoring() {
        if perms.inputMonitoringGranted { return }
        if inputMonitoringPromptShown {
            perms.openInputMonitoringSettings()
        } else {
            perms.requestInputMonitoring()
            withAnimation { inputMonitoringPromptShown = true }
        }
    }
}

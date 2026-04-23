import AVFoundation
import SwiftUI

struct PermissionsStepView: View {
    @ObservedObject var appState: AppState
    @ObservedObject private var perms = PermissionsManager.shared

    @State private var micRequestInFlight = false
    @State private var accessibilityPromptShown = false
    @State private var inputMonitoringPromptShown = false

    var body: some View {
        ScrollView(.vertical, showsIndicators: false) {
        VStack(spacing: 16) {
            Text("Permissions")
                .font(.system(size: 26, weight: .bold))

            Text("Dimmy needs access to your microphone and to the active app so it can paste transcribed text.")
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

                if appState.shortcut.isFnOnly {
                    permissionRow(
                        icon: "keyboard",
                        title: "Input Monitoring",
                        description: "Required for your Fn-key shortcut",
                        granted: perms.inputMonitoringGranted,
                        pending: perms.inputMonitoring == kIOHIDAccessTypeUnknown && !inputMonitoringPromptShown,
                        action: requestInputMonitoring
                    )
                }
            }
            .padding(.horizontal, 20)

            if !perms.microphoneGranted {
                Text("Microphone is required. The global shortcut won't work without Accessibility either.")
                    .font(.system(size: 11))
                    .foregroundColor(Color(nsColor: .tertiaryLabelColor))
                    .multilineTextAlignment(.center)
                    .padding(.horizontal, 24)
                    .padding(.top, 4)
            } else if !perms.allRequiredGranted {
                Text("Accessibility can be granted later, but the global shortcut won't work without it.")
                    .font(.system(size: 11))
                    .foregroundColor(Color(nsColor: .tertiaryLabelColor))
                    .multilineTextAlignment(.center)
                    .padding(.horizontal, 24)
                    .padding(.top, 4)
            }

            if accessibilityPromptShown && !perms.accessibilityGranted {
                stuckHelpView
                    .padding(.horizontal, 20)
                    .padding(.top, 6)
            }

            Spacer().frame(height: 8)
        }
        .padding(.horizontal, 28)
        .padding(.vertical, 16)
        }
        .onAppear { perms.refresh() }
    }

    // MARK: - Stuck help (for stale TCC entries on ad-hoc signed dev builds)

    /// Shown when the user has already been prompted but Accessibility still reads as not granted —
    /// classic symptom of a signature mismatch between the running binary and an older TCC entry.
    /// Running `tccutil reset` clears stale entries so the next grant creates a fresh one.
    private var stuckHelpView: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("Dimmy looks enabled in Settings but still isn't detected?")
                .font(.system(size: 12, weight: .medium))
            Text("macOS may have kept a stale entry from a previous build. Reset Dimmy's Accessibility grant and try again.")
                .font(.system(size: 11))
                .foregroundColor(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            Button("Reset and re-grant Accessibility") {
                perms.resetTccEntries(services: ["Accessibility"])
                accessibilityPromptShown = false
            }
            .buttonStyle(.bordered)
            .controlSize(.small)
        }
        .padding(10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            RoundedRectangle(cornerRadius: 8)
                .fill(Color.secondary.opacity(0.08))
        )
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

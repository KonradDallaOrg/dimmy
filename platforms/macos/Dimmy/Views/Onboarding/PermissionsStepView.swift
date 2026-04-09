import SwiftUI
import AVFoundation

struct PermissionsStepView: View {
    @ObservedObject var appState: AppState
    let onContinue: () -> Void
    @State private var pollTimer: Timer?
    @State private var accessibilityOpened = false

    var body: some View {
        VStack(spacing: 20) {
            Spacer()

            Text("Permissions")
                .font(.system(size: 28, weight: .bold))

            Text("Dimmy needs two permissions to work")
                .font(.system(size: 14))
                .foregroundColor(.secondary)

            VStack(spacing: 16) {
                permissionRow(
                    icon: "mic.fill",
                    title: "Microphone",
                    description: "To hear your voice",
                    granted: appState.micPermissionGranted,
                    action: requestMicrophonePermission
                )

                permissionRow(
                    icon: "hand.raised.fill",
                    title: "Accessibility",
                    description: "To paste text in the active app",
                    granted: appState.accessibilityPermissionGranted,
                    action: openAccessibilitySettings
                )
            }
            .padding(.horizontal, 20)

            // Hint when accessibility settings were opened but not yet granted
            if accessibilityOpened && !appState.accessibilityPermissionGranted {
                HStack(spacing: 10) {
                    Image(systemName: "arrow.up.right.square")
                        .foregroundColor(.orange)
                        .font(.system(size: 16))
                    Text("Find **Dimmy** in the list and toggle it **ON**")
                        .font(.system(size: 13))
                        .foregroundColor(.secondary)
                }
                .padding(14)
                .background(
                    RoundedRectangle(cornerRadius: 10)
                        .fill(Color.orange.opacity(0.1))
                )
                .padding(.horizontal, 20)
                .transition(.opacity.combined(with: .move(edge: .top)))
            }

            // Warning when mic not granted
            if !appState.micPermissionGranted && !AppState.skipPermissions {
                HStack(spacing: 8) {
                    Image(systemName: "exclamationmark.triangle.fill")
                        .foregroundColor(.yellow)
                        .font(.system(size: 14))
                    Text("Some features won't work without microphone access.")
                        .font(.system(size: 12))
                        .foregroundColor(.secondary)
                }
                .padding(10)
                .background(
                    RoundedRectangle(cornerRadius: 8)
                        .fill(Color.yellow.opacity(0.08))
                )
                .padding(.horizontal, 20)
            }

            Spacer()

            Button(action: onContinue) {
                Text("Continue")
                    .font(.system(size: 15, weight: .semibold))
                    .frame(maxWidth: 220)
            }
            .buttonStyle(.borderedProminent)
            .controlSize(.large)

            if !appState.accessibilityPermissionGranted && !accessibilityOpened {
                Text("You can grant Accessibility later")
                    .font(.system(size: 11))
                    .foregroundColor(Color(nsColor: .tertiaryLabelColor))
            }

            Spacer().frame(height: 16)
        }
        .padding(.horizontal, 40)
        .onAppear {
            checkPermissions()
            startPolling()
        }
        .onDisappear {
            pollTimer?.invalidate()
            pollTimer = nil
        }
    }

    private func permissionRow(icon: String, title: String, description: String, granted: Bool, action: @escaping () -> Void) -> some View {
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

            if granted || AppState.skipPermissions {
                Image(systemName: "checkmark.circle.fill")
                    .font(.system(size: 22))
                    .foregroundColor(.green)
                    .transition(.scale.combined(with: .opacity))
            } else {
                Button("Grant") {
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

    private func requestMicrophonePermission() {
        AVCaptureDevice.requestAccess(for: .audio) { granted in
            Task { @MainActor in
                appState.micPermissionGranted = granted
            }
        }
    }

    private func openAccessibilitySettings() {
        // This triggers the native macOS prompt to add the app to Accessibility
        let options = [kAXTrustedCheckOptionPrompt.takeRetainedValue(): true] as CFDictionary
        let trusted = AXIsProcessTrustedWithOptions(options)

        if trusted {
            appState.accessibilityPermissionGranted = true
        } else {
            withAnimation {
                accessibilityOpened = true
            }
        }
    }

    private func startPolling() {
        pollTimer?.invalidate()
        pollTimer = Timer.scheduledTimer(withTimeInterval: 0.5, repeats: true) { _ in
            Task { @MainActor in
                checkPermissions()
            }
        }
    }

    private func checkPermissions() {
        if AppState.skipPermissions {
            appState.micPermissionGranted = true
            appState.accessibilityPermissionGranted = true
            return
        }
        switch AVCaptureDevice.authorizationStatus(for: .audio) {
        case .authorized:
            appState.micPermissionGranted = true
        default:
            appState.micPermissionGranted = false
        }
        appState.accessibilityPermissionGranted = AXIsProcessTrusted()
    }
}

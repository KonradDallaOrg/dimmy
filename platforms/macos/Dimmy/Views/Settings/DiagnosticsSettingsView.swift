import AppKit
import SwiftUI

struct DiagnosticsSettingsView: View {
    @ObservedObject var appState: AppState
    @ObservedObject private var perms = PermissionsManager.shared

    var body: some View {
        Form {
            Section("Bundle") {
                row("Path", Bundle.main.bundlePath)
                row("Identifier", Bundle.main.bundleIdentifier ?? "—")
                row("Version", bundleVersionString)
            }

            Section("Permissions (TCC)") {
                tccRow("Microphone", granted: perms.microphoneGranted,
                       detail: "\(perms.microphone.rawValue)")
                tccRow("Accessibility", granted: perms.accessibilityGranted,
                       detail: perms.accessibilityGranted ? "trusted" : "not trusted")
                tccRow("Input Monitoring", granted: perms.inputMonitoringGranted,
                       detail: inputMonitoringDescription)
            }

            // Which device, and how much it has. The pane reported STT
            // mode and model but never what they were running on — the
            // first thing anyone reading a diagnostics page wants, and the
            // number that explains why a model spilled to the CPU.
            Section("Graphics") {
                let hw = DimmyCore.shared.hardwareInfo()
                row("Device", hw?.name ?? "not detected")
                row("Memory", hw.flatMap { $0.vramMB }.map { "\($0) MB" } ?? "—")
                row("Unified", hw?.appleSilicon == true ? "yes (Apple silicon)" : "no")
                row("Local models", hw?.fitness ?? "unknown")
            }

            Section("Hotkey") {
                row("Status", hotkeyDescription)
                row("Shortcut", appState.shortcut.displayString)
                row("Mode", appState.preferredMode.rawValue)
            }

            Section("Core") {
                row("STT Mode", appState.sttMode)
                row("Local Model", appState.localModel)
                row("Has API key", appState.hasKey ? "yes" : "no")
                row("Recording State", recordingStateDescription)
                row("Last Error", appState.lastError ?? "—")
            }

            Section("Actions") {
                Button("Refresh permissions now") { perms.refreshNow() }
                Button("Open /tmp/dimmy-hotkey.log") { openLog() }
                Button("Reset onboarding") { resetOnboarding() }
            }
        }
        .formStyle(.grouped)
        .onAppear { perms.refreshNow() }
    }

    private func row(_ label: String, _ value: String) -> some View {
        HStack(alignment: .top) {
            Text(label)
                .font(.system(size: 12))
                .foregroundColor(.secondary)
                .frame(width: 140, alignment: .leading)
            Text(value)
                .font(.system(size: 12, design: .monospaced))
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    private func tccRow(_ label: String, granted: Bool, detail: String) -> some View {
        HStack(alignment: .top) {
            Text(label)
                .font(.system(size: 12))
                .foregroundColor(.secondary)
                .frame(width: 140, alignment: .leading)
            Image(systemName: granted ? "checkmark.circle.fill" : "xmark.circle.fill")
                .foregroundColor(granted ? .green : .orange)
            Text(detail)
                .font(.system(size: 12, design: .monospaced))
                .foregroundColor(granted ? .primary : .secondary)
        }
    }

    private var bundleVersionString: String {
        let short = Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "?"
        let build = Bundle.main.infoDictionary?["CFBundleVersion"] as? String ?? "?"
        return "\(short) (build \(build))"
    }

    private var hotkeyDescription: String {
        switch appState.hotkeyStatus {
        case .uninstalled: return "uninstalled"
        case .installed: return "installed (CGEventTap active)"
        case .accessibilityMissing: return "accessibility missing"
        case .tapFailed(let reason): return "tap failed: \(reason)"
        }
    }

    private var recordingStateDescription: String {
        switch appState.recordingState {
        case .idle: return "idle"
        case .recording(let mode): return "recording (\(mode.rawValue))"
        case .transcribing: return "transcribing"
        case .processing: return "processing"
        case .completing: return "completing"
        }
    }

    private var inputMonitoringDescription: String {
        switch perms.inputMonitoring {
        case kIOHIDAccessTypeGranted: return "granted"
        case kIOHIDAccessTypeDenied: return "denied"
        case kIOHIDAccessTypeUnknown: return "unknown (not yet requested)"
        default: return "other(\(perms.inputMonitoring.rawValue))"
        }
    }

    private func openLog() {
        let url = URL(fileURLWithPath: "/tmp/dimmy-hotkey.log")
        NSWorkspace.shared.open(url)
    }

    private func resetOnboarding() {
        appState.isOnboardingComplete = false
        NSApp.keyWindow?.close()
        AppDelegate.shared?.reopenOnboarding()
    }
}

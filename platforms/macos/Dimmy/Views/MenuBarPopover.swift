import SwiftUI

struct MenuBarPopover: View {
    @ObservedObject var appState: AppState

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            // Status
            HStack {
                Circle()
                    .fill(appState.isRecording ? Color.red : Color.green)
                    .frame(width: 8, height: 8)
                Text(statusText)
                    .font(.system(size: 13, weight: .medium))
                Spacer()
            }

            Divider()

            // Info rows
            infoRow(label: "Language", value: appState.selectedLanguage.isEmpty ? "Auto" : appState.selectedLanguage)
            infoRow(label: "Provider", value: appState.sttProvider.displayName)
            infoRow(label: "LLM Style", value: appState.llmStyleEnum.displayName)
            infoRow(label: "Mode", value: appState.preferredMode.rawValue)

            HStack {
                Text("Shortcut")
                    .font(.system(size: 12))
                    .foregroundColor(.secondary)
                Spacer()
                Text(appState.shortcut.displayString)
                    .font(.system(size: 12, weight: .medium, design: .monospaced))
                    .foregroundColor(.primary)
            }

            Divider()

            // Settings
            Button("Settings...") {
                AppDelegate.shared?.openSettings()
            }
            .buttonStyle(.plain)
            .font(.system(size: 13))

            Button("Quit Dimmy") {
                NSApplication.shared.terminate(nil)
            }
            .buttonStyle(.plain)
            .font(.system(size: 13))
            .foregroundColor(.secondary)
        }
        .padding(16)
        .frame(width: 220)
    }

    private var statusText: String {
        switch appState.recordingState {
        case .idle: return "Ready"
        case .recording(.pushToTalk): return "Recording (hold)..."
        case .recording(.toggle): return "Recording..."
        case .transcribing: return "Transcribing..."
        case .processing: return "Processing..."
        case .completing: return "Done!"
        }
    }

    private func infoRow(label: String, value: String) -> some View {
        HStack {
            Text(label)
                .font(.system(size: 12))
                .foregroundColor(.secondary)
            Spacer()
            Text(value)
                .font(.system(size: 12, weight: .medium))
                .foregroundColor(.primary)
        }
    }
}

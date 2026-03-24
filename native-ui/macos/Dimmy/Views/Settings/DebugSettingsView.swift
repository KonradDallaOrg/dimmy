import SwiftUI

struct DebugSettingsView: View {
    @ObservedObject var appState: AppState
    @State private var isSimulating = false

    var body: some View {
        Form {
            Section("Test Recording") {
                Button(isSimulating ? "Simulating..." : "Simulate Full Recording Cycle") {
                    guard !isSimulating else { return }
                    isSimulating = true
                    RecordingSimulator.simulate(appState: appState)
                    DispatchQueue.main.asyncAfter(deadline: .now() + 8.0) {
                        isSimulating = false
                    }
                }
                .disabled(isSimulating)

                Text("Cycles through: Recording (3s with waveform) → Transcribing → Processing → Completing → Idle. Tests border style, waveform style, and all pill states without needing a microphone.")
                    .font(.system(size: 11))
                    .foregroundColor(.secondary)
            }

            Section("Debug Options") {
                Toggle("Audio debug logging", isOn: $appState.audioDebugEnabled)
                    .onChange(of: appState.audioDebugEnabled) { _, _ in
                        DimmyCore.shared.setConfig(appState.toRustConfig())
                    }

                Toggle("LLM request logging", isOn: $appState.llmLogEnabled)
                    .onChange(of: appState.llmLogEnabled) { _, _ in
                        DimmyCore.shared.setConfig(appState.toRustConfig())
                    }
            }

            Section("Audio Health") {
                Button("Check Audio Devices") {
                    appState.devices = DimmyCore.shared.listDevices()
                    if let health = DimmyCore.shared.checkAudioHealth() {
                        audioHealthInfo = health
                    }
                }

                if let health = audioHealthInfo {
                    HStack {
                        Text("Devices found")
                        Spacer()
                        Text("\(health["device_count"] as? Int ?? 0)")
                            .foregroundColor(.secondary)
                    }
                    HStack {
                        Text("Default device")
                        Spacer()
                        Text(health["default_device"] as? String ?? "none")
                            .foregroundColor(.secondary)
                            .lineLimit(1)
                    }
                    HStack {
                        Text("Can open stream")
                        Spacer()
                        Image(systemName: (health["can_open_stream"] as? Bool ?? false) ? "checkmark.circle.fill" : "xmark.circle")
                            .foregroundColor((health["can_open_stream"] as? Bool ?? false) ? .green : .red)
                    }
                    if let error = health["error"] as? String {
                        Text("Error: \(error)")
                            .font(.system(size: 11))
                            .foregroundColor(.red)
                    }
                }
            }

            Section("Current State") {
                infoRow("Recording State", recordingStateText)
                infoRow("STT Provider", appState.sttProvider.displayName)
                infoRow("API URL", appState.apiUrl)
                infoRow("Model", appState.apiModel)
                infoRow("Has API Key", appState.hasKey ? "Yes" : "No")
                infoRow("Border Style", appState.borderStyle)
                infoRow("Waveform Style", appState.waveformStyle)
                infoRow("LLM Style", appState.llmStyleEnum.displayName)
                infoRow("Language", appState.selectedLanguage)
            }
        }
        .formStyle(.grouped)
    }

    @State private var audioHealthInfo: [String: Any]?

    private func infoRow(_ label: String, _ value: String) -> some View {
        HStack {
            Text(label)
            Spacer()
            Text(value)
                .foregroundColor(.secondary)
                .lineLimit(1)
                .truncationMode(.middle)
        }
    }

    private var recordingStateText: String {
        switch appState.recordingState {
        case .idle: return "Idle"
        case .recording(let mode): return "Recording (\(mode.rawValue))"
        case .transcribing: return "Transcribing"
        case .processing: return "Processing"
        case .completing: return "Completing"
        }
    }

}

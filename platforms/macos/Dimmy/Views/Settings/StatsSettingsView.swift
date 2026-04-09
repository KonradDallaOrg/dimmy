import SwiftUI

struct StatsSettingsView: View {
    @ObservedObject var appState: AppState

    /// Time saved estimate: typing ~40 WPM vs dictation ~150 WPM
    /// Same formula as Windows: words * (1/40 - 1/150) * 60 seconds
    private var timeSavedSecs: Double {
        Double(appState.statsTotalWords) * (1.0 / 40.0 - 1.0 / 150.0) * 60.0
    }

    var body: some View {
        Form {
            Section("Usage Statistics") {
                statRow(
                    icon: "text.word.spacing",
                    title: "Total words dictated",
                    value: "\(appState.statsTotalWords)"
                )

                statRow(
                    icon: "timer",
                    title: "Total speaking time",
                    value: formatDuration(appState.statsTotalSpeakingSecs)
                )

                statRow(
                    icon: "clock.arrow.circlepath",
                    title: "Estimated time saved",
                    value: formatDuration(timeSavedSecs)
                )

                Text("Based on average typing speed (~40 WPM) vs dictation (~150 WPM)")
                    .font(.system(size: 11))
                    .foregroundColor(Color(nsColor: .tertiaryLabelColor))
            }
        }
        .formStyle(.grouped)
        .onAppear {
            // Refresh stats from Rust
            if let config = DimmyCore.shared.getConfig() {
                appState.loadFromRustConfig(config)
            }
        }
    }

    private func statRow(icon: String, title: String, value: String) -> some View {
        HStack {
            Label(title, systemImage: icon)
            Spacer()
            Text(value)
                .font(.system(size: 16, weight: .semibold))
                .monospacedDigit()
                .foregroundColor(.primary)
        }
        .padding(.vertical, 2)
    }

    private func formatDuration(_ seconds: Double) -> String {
        guard seconds > 0 else { return "0s" }
        let h = Int(seconds) / 3600
        let m = (Int(seconds) % 3600) / 60
        let s = Int(seconds) % 60
        if h > 0 {
            return String(format: "%dh %02dm", h, m)
        } else if m > 0 {
            return String(format: "%dm %02ds", m, s)
        } else {
            return String(format: "%ds", s)
        }
    }
}

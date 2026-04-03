import SwiftUI

struct OverlaySettingsView: View {
    @ObservedObject var appState: AppState

    private let positions = ["Top Right", "Top Left", "Bottom Right", "Bottom Left", "Bottom Center", "Top Center"]
    // Match Windows border styles exactly
    private let borderStyles = ["Rainbow", "Blue", "Green", "Purple", "Orange", "None"]
    // Match Windows waveform styles exactly
    private let waveformStyles = ["Bars", "Bars Center", "Bars Round", "Line", "Dots"]

    var body: some View {
        Form {
            Section("Pill Overlay") {
                Picker("Default position", selection: $appState.overlayPosition) {
                    ForEach(positions, id: \.self) { pos in
                        Text(pos).tag(pos)
                    }
                }
                .onChange(of: appState.overlayPosition) { _, _ in syncConfigToRust() }

                Button("Reset Position") {
                    UserDefaults.standard.removeObject(forKey: "pillX")
                    UserDefaults.standard.removeObject(forKey: "pillY")
                    appState.pillPosition = nil
                }

                Text("You can always drag the pill to reposition it")
                    .font(.system(size: 11))
                    .foregroundColor(.secondary)
            }

            Section("Recording Animation") {
                Picker("Border style", selection: $appState.borderStyle) {
                    ForEach(borderStyles, id: \.self) { style in
                        Text(style).tag(style)
                    }
                }
                .onChange(of: appState.borderStyle) { _, _ in syncConfigToRust() }

                Picker("Waveform style", selection: $appState.waveformStyle) {
                    ForEach(waveformStyles, id: \.self) { style in
                        Text(style).tag(style)
                    }
                }
                .onChange(of: appState.waveformStyle) { _, _ in syncConfigToRust() }
            }
        }
        .formStyle(.grouped)
    }

    private func syncConfigToRust() {
        DimmyCore.shared.setConfig(appState.toRustConfig())
    }
}

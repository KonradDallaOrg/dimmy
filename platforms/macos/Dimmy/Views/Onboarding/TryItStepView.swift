import SwiftUI

struct TryItStepView: View {
    @ObservedObject var appState: AppState
    let onComplete: () -> Void

    @State private var demoText: String = ""
    @State private var hasTriedRecording = false
    @State private var showSuccess = false

    var body: some View {
        VStack(spacing: 20) {
            Spacer()

            if showSuccess {
                successView
            } else {
                tryView
            }

            Spacer()
        }
        .padding(.horizontal, 40)
        .onChange(of: appState.recordingState) { _, newState in
            if case .completing = newState {
                // Use real transcript from Rust, fall back to placeholder
                demoText = appState.lastTranscript.isEmpty ? "No speech detected" : appState.lastTranscript
                hasTriedRecording = true
            }
            if case .idle = newState, hasTriedRecording {
                withAnimation(.spring(response: 0.4)) {
                    showSuccess = true
                }
            }
        }
    }

    private var tryView: some View {
        VStack(spacing: 20) {
            Text("Try it!")
                .font(.system(size: 28, weight: .bold))

            Text("Hold \(appState.shortcut.displayString) and say something")
                .font(.system(size: 14))
                .foregroundColor(.secondary)

            Text("Look at the pill overlay — it will animate while you speak")
                .font(.system(size: 12))
                .foregroundColor(Color(nsColor: .tertiaryLabelColor))

            // Demo text field
            VStack(alignment: .leading, spacing: 6) {
                Text("Your dictation will appear here:")
                    .font(.system(size: 11))
                    .foregroundColor(Color(nsColor: .tertiaryLabelColor))

                ZStack(alignment: .topLeading) {
                    RoundedRectangle(cornerRadius: 8)
                        .fill(Color(nsColor: .textBackgroundColor))
                        .frame(height: 80)

                    if demoText.isEmpty {
                        Text("Waiting for your voice...")
                            .font(.system(size: 13))
                            .foregroundColor(Color(nsColor: .tertiaryLabelColor))
                            .padding(10)
                    } else {
                        Text(demoText)
                            .font(.system(size: 13))
                            .padding(10)
                    }
                }
                .overlay(
                    RoundedRectangle(cornerRadius: 8)
                        .stroke(Color.primary.opacity(0.1), lineWidth: 1)
                )
            }

            Button(action: {
                withAnimation(.spring(response: 0.4)) {
                    showSuccess = true
                }
            }) {
                Text("Skip for now")
                    .font(.system(size: 12))
                    .foregroundColor(.secondary)
            }
            .buttonStyle(.plain)
            .padding(.top, 4)
        }
    }

    private var successView: some View {
        VStack(spacing: 20) {
            Image(systemName: "checkmark.circle.fill")
                .font(.system(size: 56))
                .foregroundColor(.green)

            Text("You're all set!")
                .font(.system(size: 24, weight: .bold))

            Text("Dimmy lives in your menu bar.\nHold \(appState.shortcut.displayString) anywhere to dictate.")
                .font(.system(size: 13))
                .foregroundColor(.secondary)
                .multilineTextAlignment(.center)
                .lineSpacing(4)

            Button(action: {
                appState.showPillIntro = true
                onComplete()
            }) {
                Text("Start Using Dimmy")
                    .font(.system(size: 14, weight: .semibold))
                    .frame(maxWidth: 200)
            }
            .buttonStyle(.borderedProminent)
            .controlSize(.large)
        }
    }
}
